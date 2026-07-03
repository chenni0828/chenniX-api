//! Channel/key router.
//!
//! Given a list of `(channel, keys, upstream_model_name, priority, weight)`
//! tuples for a single model, plus the caller's user_group, a key-availability
//! predicate, a routing strategy and a small-model quota predicate,
//! `Router::route` produces a flat, ordered list of `RoutedKey` candidates
//! that the executor should try in order.
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
//! ## Strategy
//! - **Priority**: candidates are sorted by (cost_tier, binding_priority,
//!   key_priority, -quota_ratio, price) and tried in that fixed order.
//! - **LoadBalance**: candidates are emitted in weighted-random order
//!   (weight per binding); on each step a candidate is drawn proportional to
//!   its weight from the remaining pool. Within equal weights, lower
//!   `binding_priority` breaks ties stably.

use chennix_common::{ChannelConfig, CostTier, KeyConfig};

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
    /// Filter, expand and order the channels for a single model into a flat
    /// list of `RoutedKey`s the executor can walk in order.
    ///
    /// See the module docs for the filtering order and the strategy branches.
    pub fn route(
        channels: Vec<(ChannelConfig, Vec<KeyConfig>, String, i32, i32)>,
        user_group: &str,
        is_key_available: impl Fn(i64) -> bool,
        strategy: RoutingStrategy,
        quota_filter: impl Fn(i64, &str) -> bool,
    ) -> Vec<RoutedKey> {
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

/// Priority mode: flatten + sort by the established multi-key ordering.
fn route_priority(groups: Vec<Vec<RoutedKey>>) -> Vec<RoutedKey> {
    let mut candidates: Vec<RoutedKey> = groups.into_iter().flatten().collect();
    candidates.sort_by(|a, b| {
        // cost_tier: Free before Paid
        let a_tier = tier_rank(a.key.cost_tier);
        let b_tier = tier_rank(b.key.cost_tier);
        a_tier
            .cmp(&b_tier)
            .then(a.binding_priority.cmp(&b.binding_priority))
            .then(a.key.key_priority.cmp(&b.key.key_priority))
            .then(
                quota_remaining_ratio(&b.key)
                    .partial_cmp(&quota_remaining_ratio(&a.key))
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                price_rank(&a.key.price_per_1k_tokens)
                    .cmp(&price_rank(&b.key.price_per_1k_tokens)),
            )
    });
    candidates
}

/// LoadBalance mode: weighted-random without replacement.
///
/// Candidates are pre-sorted by `binding_priority` ascending so that, within
/// equal-weight groups, the draw walks lower-priority candidates first
/// (stable tie-break). Each step draws a candidate proportional to its
/// weight (`weight < 1` is clamped to 1) from the remaining pool, removes
/// it, and repeats — so the emitted order is exactly "weighted random from
/// the remaining candidates" on each step, matching the executor's
/// fail-and-try-next loop.
fn route_load_balance(groups: Vec<Vec<RoutedKey>>) -> Vec<RoutedKey> {
    use rand::Rng;

    let mut candidates: Vec<RoutedKey> = groups.into_iter().flatten().collect();
    if candidates.is_empty() {
        return candidates;
    }
    candidates.sort_by(|a, b| a.binding_priority.cmp(&b.binding_priority));

    let mut rng = rand::thread_rng();
    let mut out: Vec<RoutedKey> = Vec::with_capacity(candidates.len());
    while !candidates.is_empty() {
        // total >= 1: pool is non-empty and weights are clamped to >= 1.
        let total: i64 = candidates.iter().map(|c| c.weight.max(1) as i64).sum();
        let pick = rng.gen_range(0..total);
        let mut acc: i64 = 0;
        let mut chosen = candidates.len() - 1;
        for (i, c) in candidates.iter().enumerate() {
            acc += c.weight.max(1) as i64;
            if pick < acc {
                chosen = i;
                break;
            }
        }
        out.push(candidates.remove(chosen));
    }
    out
}

/// Free = 0 (sorts first), Paid = 1.
fn tier_rank(t: CostTier) -> u8 {
    match t {
        CostTier::Free => 0,
        CostTier::Paid => 1,
    }
}

/// `None` price (unknown) sorts after `Some(_)` so free-tier / priced keys
/// come first. Within `Some`, cheaper sorts first — that falls out of f64
/// total ordering. To get a stable `Ord`, we convert to bits.
fn price_rank(price: &Option<f64>) -> u64 {
    match price {
        None => u64::MAX,
        Some(p) => {
            if p.is_nan() || *p < 0.0 {
                // treat invalid as "unknown"
                u64::MAX
            } else {
                // f64 to bits preserves total ordering for non-negative
                // non-NaN floats (IEEE 754 layout).
                p.to_bits()
            }
        }
    }
}

/// `used_quota / free_quota` clamped to `[0, 1]`. Higher ratio = closer
/// to exhaustion = should sort later. Keys with no `free_quota` are
/// treated as fully unused (ratio 0).
fn quota_remaining_ratio(k: &KeyConfig) -> f64 {
    match k.free_quota {
        Some(total) if total > 0 => {
            let used = k.used_quota.min(total);
            // remaining ratio = (total - used) / total
            (total - used) as f64 / total as f64
        }
        _ => 1.0, // no quota limit → "fully remaining"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chennix_common::{ChannelProvider, KeyStatus};

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
    fn test_free_before_paid() {
        // Same binding_priority + key_priority — Free must come first.
        let ch = channel(1, "default");
        let paid = key(10, 1, CostTier::Paid, 100, Some(0.01));
        let free = key(11, 1, CostTier::Free, 100, Some(0.0));
        let input = vec![(ch.clone(), vec![paid, free], "upstream".into(), 100, 1)];

        let out = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].key.cost_tier, CostTier::Free);
        assert_eq!(out[1].key.cost_tier, CostTier::Paid);
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
        assert_eq!(out.len(), 2);
        // channel 2 has binding_priority 10 → comes first
        assert_eq!(out[0].channel.id, 2);
        assert_eq!(out[1].channel.id, 1);
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
        let mut ids: Vec<i64> = out_vip.iter().map(|r| r.channel.id).collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2]);

        // default user → ch1 + ch3
        let out_def = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        let mut ids: Vec<i64> = out_def.iter().map(|r| r.channel.id).collect();
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
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].key.id, 10);
    }

    #[test]
    fn test_sort_full_chain_tie_breakers() {
        // Two candidates: same tier, same binding_priority, same key_priority.
        // Different remaining ratio and price — ratio wins (higher ratio first).
        let ch = channel(1, "default");
        let mut k_low = key(10, 1, CostTier::Paid, 100, Some(0.01));
        k_low.free_quota = Some(1000);
        k_low.used_quota = 900; // 10% remaining

        let mut k_high = key(11, 1, CostTier::Paid, 100, Some(0.05));
        k_high.free_quota = Some(1000);
        k_high.used_quota = 100; // 90% remaining

        let input = vec![(ch, vec![k_low, k_high], "u".into(), 100, 1)];
        let out = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        assert_eq!(out.len(), 2);
        // k_high (more remaining) first, even though it's pricier
        assert_eq!(out[0].key.id, 11);
        assert_eq!(out[1].key.id, 10);
    }

    #[test]
    fn test_price_tiebreaker_when_ratio_equal() {
        let ch = channel(1, "default");
        let k_cheap = key(10, 1, CostTier::Paid, 100, Some(0.01));
        let k_pricey = key(11, 1, CostTier::Paid, 100, Some(0.50));
        let input = vec![(ch, vec![k_pricey, k_cheap], "u".into(), 100, 1)];

        let out = Router::route(input, "default", all_available, RoutingStrategy::Priority, quota_ok);
        assert_eq!(out[0].key.id, 10); // cheap first
        assert_eq!(out[1].key.id, 11);
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
        assert_eq!(out[0].weight, 7);
        assert_eq!(out[0].binding_priority, 50);
    }

    #[test]
    fn test_load_balance_prefers_higher_weight() {
        // Two bindings with weights 1 and 9. The weight-9 candidate should
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
            if out[0].key.id == 11 {
                first_b += 1;
            }
        }
        // Expected ~1800/2000. Threshold 1400 (70%) avoids flakiness.
        assert!(
            first_b > 1400,
            "weight-9 candidate should dominate first pick, got {first_b}/2000"
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
        let mut ids: Vec<i64> = out.iter().map(|r| r.key.id).collect();
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
        assert_eq!(out[0].channel.id, 2);
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
        assert_eq!(out[0].channel.id, 2);
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
        assert_eq!(out[0].channel.id, 2);
    }
}
