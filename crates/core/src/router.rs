//! Channel/key router.
//!
//! Given a list of `(channel, keys, upstream_model_name, priority, weight)`
//! tuples for a single model, plus the caller's user_group, a key-availability
//! predicate, a routing strategy and a small-model quota predicate,
//! `Router::route` produces a **binding-grouped** list of candidate lists
//! `Vec<Vec<RoutedKey>>` that the executor walks outer = binding, inner = key.
//!
//! ## Filtering order (per binding)
//! 1. **user_group**: only channels whose comma-separated `group` field
//!    contains `user_group` are kept.
//! 2. **quota_filter**: drop the whole binding if `quota_filter(channel_id,
//!    upstream_model_name)` returns false (small-model quota exhausted).
//! 3. **is_key_available**: a key is dropped if `is_key_available(key.id)`
//!    returns false (cooldown / disabled / quota-exhausted at the runtime
//!    layer). If a binding's channel ends up with no surviving keys, the
//!    binding is dropped and the next candidate is tried.
//!
//! ## Strategy (binding-level)
//! - **Priority**: bindings are sorted by `binding_priority` ascending; keys
//!   inside a binding are sorted by `key_priority` (+ quota ratio, price).
//! - **LoadBalance**: bindings are emitted in weighted-random order
//!   (weight per binding); keys inside a binding are always sorted by
//!   `key_priority` (key layer is Priority-only by design).
//!
//! The key layer has no LoadBalance — `key_priority` (set by drag-and-drop
//! in the admin UI) fully determines intra-binding order.

use chennix_common::{ChannelConfig, KeyConfig};

use crate::cache::RoutingStrategy;

/// A single candidate the executor may attempt.
///
/// `upstream_model_name` is the per-binding name that must be substituted
/// into the outgoing request's `model` field before calling the adaptor.
/// `binding_priority` is the per-binding priority (lower = tried first),
/// configured on the model management page. `weight` is the per-binding
/// load-balance weight (only consulted under `RoutingStrategy::LoadBalance`).
#[derive(Debug, Clone)]
pub struct RoutedKey {
    pub channel: ChannelConfig,
    pub key: KeyConfig,
    pub upstream_model_name: String,
    pub binding_priority: i32,
    pub weight: i32,
}

pub struct Router;

impl Router {
    /// Filter, expand and order the channels for a single model into a
    /// **binding-grouped** list of candidate lists `Vec<Vec<RoutedKey>>`
    /// that the executor walks outer = binding, inner = key.
    ///
    /// See the module docs for the filtering order and the strategy branches.
    pub fn route(
        channels: Vec<(ChannelConfig, Vec<KeyConfig>, String, i32, i32)>,
        user_group: &str,
        is_key_available: impl Fn(i64) -> bool,
        strategy: RoutingStrategy,
        quota_filter: impl Fn(i64, &str) -> bool,
    ) -> Vec<Vec<RoutedKey>> {
        let group = user_group.trim();
        let group_match = |ch_group: &str| {
            ch_group.split(',').any(|g| g.trim() == group)
        };

        // Build per-binding candidate buckets. Each surviving binding yields
        // one or more `RoutedKey`s (one per available key); bindings whose
        // channel has no available key produce an empty bucket and are
        // dropped so the next candidate is tried.
        let mut groups: Vec<Vec<RoutedKey>> = Vec::new();
        for (channel, keys, upstream_model_name, binding_priority, weight) in channels {
            // 1. group filter
            if !group_match(&channel.group) {
                continue;
            }
            // 2. quota_filter — drop exhausted small models
            if !quota_filter(channel.id, &upstream_model_name) {
                continue;
            }
            // 3. expand + key availability filter
            let mut bucket: Vec<RoutedKey> = Vec::new();
            for key in keys {
                if !is_key_available(key.id) {
                    continue;
                }
                bucket.push(RoutedKey {
                    channel: channel.clone(),
                    key,
                    upstream_model_name: upstream_model_name.clone(),
                    binding_priority,
                    weight,
                });
            }
            // 4. binding with no available key → skip, try next candidate
            if bucket.is_empty() {
                continue;
            }
            groups.push(bucket);
        }

        match strategy {
            RoutingStrategy::Priority => route_priority(groups),
            RoutingStrategy::LoadBalance => route_load_balance(groups),
        }
    }
}

/// Priority mode: two-layer sort.
///
/// 1. Each binding's keys are sorted by `key_priority` only. When priorities
///    are equal, the original database order (stable sort) is preserved.
/// 2. Bindings are sorted by `binding_priority` ascending (lower = tried
///    first).
fn route_priority(mut groups: Vec<Vec<RoutedKey>>) -> Vec<Vec<RoutedKey>> {
    for bucket in &mut groups {
        bucket.sort_by(|a, b| a.key.key_priority.cmp(&b.key.key_priority));
    }
    groups.sort_by(|a, b| {
        a.first()
            .map(|r| r.binding_priority)
            .unwrap_or(i32::MAX)
            .cmp(&b.first().map(|r| r.binding_priority).unwrap_or(i32::MAX))
    });
    groups
}

/// LoadBalance mode: binding-level weighted-random without replacement.
///
/// 1. Each binding's keys are sorted by `key_priority` (key layer is always
///    Priority by design — no LoadBalance at the key layer).
/// 2. Bindings are pre-sorted by `binding_priority` ascending so that, within
///    equal-weight groups, the draw walks lower-priority bindings first
///    (stable tie-break).
/// 3. Each step draws a *binding* proportional to its weight (`weight < 1`
///    is clamped to 1) from the remaining pool, removes it, and repeats.
///    The executor then exhausts the binding's keys before moving to the
///    next drawn binding.
fn route_load_balance(mut groups: Vec<Vec<RoutedKey>>) -> Vec<Vec<RoutedKey>> {
    use rand::Rng;

    if groups.is_empty() {
        return groups;
    }
    // 1. key layer: always Priority
    for bucket in &mut groups {
        bucket.sort_by(|a, b| a.key.key_priority.cmp(&b.key.key_priority));
    }
    // 2. pre-sort by binding_priority (stable tiebreak for equal weights)
    groups.sort_by(|a, b| {
        a.first()
            .map(|r| r.binding_priority)
            .unwrap_or(i32::MAX)
            .cmp(&b.first().map(|r| r.binding_priority).unwrap_or(i32::MAX))
    });

    // 3. binding-level weighted random without replacement
    let binding_weight = |b: &Vec<RoutedKey>| -> i64 {
        b.first().map(|r| r.weight.max(1) as i64).unwrap_or(1)
    };
    let mut rng = rand::thread_rng();
    let mut out: Vec<Vec<RoutedKey>> = Vec::with_capacity(groups.len());
    while !groups.is_empty() {
        let total: i64 = groups.iter().map(binding_weight).sum();
        let pick = rng.gen_range(0..total);
        let mut acc: i64 = 0;
        let mut chosen = groups.len() - 1;
        for (i, b) in groups.iter().enumerate() {
            acc += binding_weight(b);
            if pick < acc {
                chosen = i;
                break;
            }
        }
        out.push(groups.remove(chosen));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chennix_common::{ChannelProvider, CostTier, KeyStatus};

    fn channel(id: i64, group: &str) -> ChannelConfig {
        ChannelConfig {
            id,
            name: format!("ch-{id}"),
            provider: ChannelProvider::OpenaiCompatible,
            base_url: format!("http://ch-{id}"),
            group: group.into(),
        }
    }

    fn key(id: i64, channel_id: i64, tier: CostTier, kp: i32, price: Option<f64>) -> KeyConfig {
        KeyConfig {
            id,
            channel_id,
            api_key: format!("sk-{id}"),
            label: None,
            cost_tier: tier,
            key_priority: kp,
            price_per_1k_tokens: price,
            free_quota: None,
            used_quota: 0,
            quota_reset_period: None,
            status: KeyStatus::Active,
        }
    }

    fn all_available(_id: i64) -> bool {
        true
    }

    fn quota_ok(_ch: i64, _up: &str) -> bool {
        true
    }

    #[test]
    fn test_key_priority_order() {
        // Same binding_priority; key_priority determines order.
        let ch = channel(1, "default");
        let k_low = key(10, 1, CostTier::Paid, 50, Some(0.01));
        let k_high = key(11, 1, CostTier::Paid, 100, Some(0.0));
        let input = vec![(ch.clone(), vec![k_high, k_low], "upstream".into(), 100, 1)];

        let out = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 2);
        assert_eq!(out[0][0].key.id, 10); // priority 50 first
        assert_eq!(out[0][1].key.id, 11);
    }

    #[test]
    fn test_priority_order() {
        // Two channels, both paid. Lower binding_priority first.
        let ch_a = channel(1, "default");
        let ch_b = channel(2, "default");
        let k_a = key(10, 1, CostTier::Paid, 100, Some(0.01));
        let k_b = key(11, 2, CostTier::Paid, 100, Some(0.01));
        let input = vec![
            (ch_a.clone(), vec![k_a.clone()], "u-a".into(), 50, 1),
            (ch_b.clone(), vec![k_b.clone()], "u-b".into(), 10, 1),
        ];

        let out = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        assert_eq!(out.len(), 2); // 2 bindings
        // channel 2 has binding_priority 10 → comes first
        assert_eq!(out[0][0].channel.id, 2);
        assert_eq!(out[1][0].channel.id, 1);
    }

    #[test]
    fn test_group_filter() {
        // ch1 allows "default,vip"; ch2 allows "vip" only; ch3 allows "default" only.
        let ch1 = channel(1, "default,vip");
        let ch2 = channel(2, "vip");
        let ch3 = channel(3, "default");

        let k1 = key(10, 1, CostTier::Paid, 100, None);
        let k2 = key(11, 2, CostTier::Paid, 100, None);
        let k3 = key(12, 3, CostTier::Paid, 100, None);

        let input = vec![
            (ch1, vec![k1], "u".into(), 100, 1),
            (ch2, vec![k2], "u".into(), 100, 1),
            (ch3, vec![k3], "u".into(), 100, 1),
        ];

        // vip user → ch1 + ch2
        let out_vip = Router::route(input.clone(), "vip", all_available, RoutingStrategy::Priority, quota_ok);
        let mut ids: Vec<i64> = out_vip.iter().flatten().map(|r| r.channel.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);

        // default user → ch1 + ch3
        let out_def = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        let mut ids: Vec<i64> = out_def.iter().flatten().map(|r| r.channel.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 3]);
    }

    #[test]
    fn test_availability_filter() {
        // Two keys; key 10 is unavailable. Should be dropped.
        let ch = channel(1, "default");
        let k_ok = key(10, 1, CostTier::Paid, 100, None);
        let k_down = key(11, 1, CostTier::Paid, 100, None);
        let input = vec![(ch, vec![k_ok.clone(), k_down], "u".into(), 100, 1)];

        let out = Router::route(input, "default", |id| id == 10, RoutingStrategy::Priority, quota_ok);
        assert_eq!(out.len(), 1); // 1 binding
        assert_eq!(out[0].len(), 1); // 1 surviving key
        assert_eq!(out[0][0].key.id, 10);
    }

    #[test]
    fn test_no_candidates_returns_empty() {
        // Group filter rejects everything.
        let ch = channel(1, "vip");
        let k = key(10, 1, CostTier::Paid, 100, None);
        let input = vec![(ch, vec![k], "u".into(), 100, 1)];

        let out = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        assert!(out.is_empty());
    }

    // ---- new strategy / quota tests ----

    #[test]
    fn test_routed_key_carries_weight_and_priority() {
        let ch = channel(1, "default");
        let k = key(10, 1, CostTier::Paid, 100, None);
        let input = vec![(ch, vec![k], "u".into(), 50, 7)];
        let out = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0].weight, 7);
        assert_eq!(out[0][0].binding_priority, 50);
    }

    #[test]
    fn test_load_balance_prefers_higher_weight() {
        // Two bindings with weights 1 and 9. The weight-9 binding should
        // be picked first the large majority of the time (~90%).
        let ch_a = channel(1, "default");
        let ch_b = channel(2, "default");
        let k_a = key(10, 1, CostTier::Paid, 100, None);
        let k_b = key(11, 2, CostTier::Paid, 100, None);
        let input = vec![
            (ch_a, vec![k_a], "u-a".into(), 100, 1),
            (ch_b, vec![k_b], "u-b".into(), 100, 9),
        ];

        let mut first_b = 0;
        for _ in 0..2000 {
            let out = Router::route(
                input.clone(),
                "default",
                all_available,
                RoutingStrategy::LoadBalance,
                quota_ok,
            );
            assert_eq!(out.len(), 2);
            if out[0][0].key.id == 11 {
                first_b += 1;
            }
        }
        // Expected ~1800/2000. Threshold 1400 (70%) avoids flakiness.
        assert!(
            first_b > 1400,
            "weight-9 binding should dominate first pick, got {first_b}/2000"
        );
    }

    #[test]
    fn test_load_balance_equal_weights_returns_all_candidates() {
        let ch_a = channel(1, "default");
        let ch_b = channel(2, "default");
        let k_a = key(10, 1, CostTier::Paid, 100, None);
        let k_b = key(11, 2, CostTier::Paid, 100, None);
        let input = vec![
            (ch_a, vec![k_a], "u-a".into(), 100, 1),
            (ch_b, vec![k_b], "u-b".into(), 100, 1),
        ];
        let out = Router::route(
            input,
            "default",
            all_available,
            RoutingStrategy::LoadBalance,
            quota_ok,
        );
        assert_eq!(out.len(), 2);
        let mut ids: Vec<i64> = out.iter().flatten().map(|r| r.key.id).collect();
        ids.sort();
        assert_eq!(ids, vec![10, 11]);
    }

    #[test]
    fn test_quota_filter_drops_exhausted_binding() {
        let ch_a = channel(1, "default");
        let ch_b = channel(2, "default");
        let k_a = key(10, 1, CostTier::Paid, 100, None);
        let k_b = key(11, 2, CostTier::Paid, 100, None);
        let input = vec![
            (ch_a, vec![k_a], "u-a".into(), 100, 1),
            (ch_b, vec![k_b], "u-b".into(), 100, 1),
        ];
        // channel 1's small model is exhausted → its binding is dropped.
        let out = Router::route(
            input,
            "default",
            all_available,
            RoutingStrategy::Priority,
            |ch_id, _up| ch_id != 1,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0].channel.id, 2);
    }

    #[test]
    fn test_quota_filter_drops_exhausted_binding_in_load_balance() {
        // Same as above but under LoadBalance; the exhausted binding must
        // never appear, so the result is deterministic regardless of weights.
        let ch_a = channel(1, "default");
        let ch_b = channel(2, "default");
        let k_a = key(10, 1, CostTier::Paid, 100, None);
        let k_b = key(11, 2, CostTier::Paid, 100, None);
        let input = vec![
            (ch_a, vec![k_a], "u-a".into(), 100, 9),
            (ch_b, vec![k_b], "u-b".into(), 100, 1),
        ];
        let out = Router::route(
            input,
            "default",
            all_available,
            RoutingStrategy::LoadBalance,
            |ch_id, _up| ch_id != 1,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0].channel.id, 2);
    }

    #[test]
    fn test_skips_binding_whose_channel_has_no_available_key() {
        // ch_a has a single key (id 10) which is unavailable; the binding
        // is dropped and only ch_b's candidate survives.
        let ch_a = channel(1, "default");
        let ch_b = channel(2, "default");
        let k_a = key(10, 1, CostTier::Paid, 100, None);
        let k_b = key(11, 2, CostTier::Paid, 100, None);
        let input = vec![
            (ch_a, vec![k_a], "u-a".into(), 100, 1),
            (ch_b, vec![k_b], "u-b".into(), 100, 1),
        ];
        let out = Router::route(
            input,
            "default",
            |id| id != 10,
            RoutingStrategy::Priority,
            quota_ok,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0].channel.id, 2);
    }
}
