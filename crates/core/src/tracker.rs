//! Usage tracker: writes durable usage rows and updates runtime health state.
//!
//! Two entry points:
//! - `track_success`: persist a usage row, bump key quota usage in storage,
//!   bump in-memory health counter, and (for bound small models with a
//!   configured quota) accumulate `discovered_models.used_quota`.
//! - `track_failure`: persist a usage row with `status=failed` and the error
//!   message; do NOT bump quota (no tokens consumed).

use async_trait::async_trait;
use chennix_common::{ProxyResult, Usage};

use crate::cache::{ConfigCache, QuotaStatus, QuotaUnit};
use crate::health::HealthManager;

/// Persistence backend the tracker talks to. Mirrors `UsageRepo` +
/// `KeyRepo::add_key_usage` + `DiscoveredModelRepo::add_discovered_model_usage`
/// so a single storage connection can implement it.
#[async_trait]
pub trait UsageWriter: Send + Sync {
    /// Insert one row into `usage_logs`.
    async fn log_usage(
        &self,
        user_id: i64,
        token_id: i64,
        channel_id: i64,
        key_id: i64,
        model_id: i64,
        usage: &Usage,
        quota_cost: i64,
        request_type: &str,
        status: &str,
        error: Option<&str>,
    ) -> ProxyResult<()>;

    /// Bump `channel_keys.used_quota` (the durable per-key counter).
    async fn add_key_usage(&self, key_id: i64, tokens: u64) -> ProxyResult<()>;

    /// Atomically bump `discovered_models.used_quota` by `delta` and set
    /// `quota_status` (the durable per-small-model counter). Best-effort:
    /// implementations should not error if the row is absent.
    async fn add_small_model_usage(
        &self,
        channel_id: i64,
        upstream_model_name: &str,
        delta: i64,
        quota_status: &str,
    ) -> ProxyResult<()>;

    /// Insert one row into `request_logs` (审计/日志表)。
    async fn log_request(
        &self,
        request_id: &str,
        client_ip: Option<&str>,
        method: &str,
        path: &str,
        client_model: Option<&str>,
        normalized_model: Option<&str>,
        channel_name: Option<&str>,
        key_label: Option<&str>,
        attempted_keys: Option<&str>,
        upstream_status: Option<i64>,
        response_status: i64,
        duration_ms: i64,
        stream: bool,
        error_message: Option<&str>,
        user_id: Option<i64>,
        token_id: Option<i64>,
        quota_cost: i64,
    ) -> ProxyResult<()>;
}

pub struct Tracker;

impl Tracker {
    /// Record a successful request: persist usage row, bump durable key
    /// counter, bump runtime health counter, and — for bound small models
    /// that have a configured quota — accumulate
    /// `discovered_models.used_quota` (incremental cache update + durable
    /// bump). Small-model accumulation is best-effort: a missing cache
    /// entry or absent quota limit silently skips it.
    pub async fn track_success(
        writer: &dyn UsageWriter,
        health: &HealthManager,
        cache: &ConfigCache,
        user_id: i64,
        token_id: i64,
        channel_id: i64,
        key_id: i64,
        model_id: i64,
        upstream_model_name: &str,
        usage: &Usage,
        quota_cost: i64,
        request_type: &str,
    ) -> ProxyResult<()> {
        // 对齐 new-api PostTextConsumeQuota / CLIProxyAPI usage.Manager：
        // track_success 内的所有写操作都是统计/日志性质（非钱，钱在
        // settle 阶段），全部 best-effort——失败只记日志不传播，避免
        // 单个计数器写入失败丢掉整条 usage 记录。
        //
        // 1. durable usage log row
        if let Err(e) = writer
            .log_usage(
                user_id,
                token_id,
                channel_id,
                key_id,
                model_id,
                usage,
                quota_cost,
                request_type,
                "success",
                None,
            )
            .await
        {
            tracing::error!("track_success: log_usage failed: {}", e);
        }
        // 2. durable per-key counter (used_quota in channel_keys table)
        if let Err(e) = writer.add_key_usage(key_id, usage.total_tokens).await {
            tracing::error!("track_success: add_key_usage failed: {}", e);
        }
        // 3. in-memory runtime counter (used for fast routing sort)
        health.add_usage(key_id, usage.total_tokens).await;
        // 4. small-model quota accumulation (best-effort; skip if not a
        //    configured small model, if quota_limit is NULL, or if the
        //    cache has no entry for this binding).
        accumulate_small_model_quota(writer, cache, channel_id, upstream_model_name, usage).await;
        Ok(())
    }

    /// Record a failed request: persist usage row with `status=failed` and
    /// the error message. Does NOT touch quota counters — no tokens were
    /// consumed upstream.
    pub async fn track_failure(
        writer: &dyn UsageWriter,
        user_id: i64,
        token_id: i64,
        channel_id: i64,
        key_id: i64,
        model_id: i64,
        request_type: &str,
        error: &str,
    ) -> ProxyResult<()> {
        let zero_usage = Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
        };
        writer
            .log_usage(
                user_id,
                token_id,
                channel_id,
                key_id,
                model_id,
                &zero_usage,
                0,
                request_type,
                "failed",
                Some(error),
            )
            .await?;
        Ok(())
    }
}

/// Accumulate `discovered_models.used_quota` for the bound small model
/// `(channel_id, upstream_model_name)`.
///
/// Rules (per spec):
/// - No cache entry for this binding → not a small model / cache not loaded → skip.
/// - `quota_limit` is `None` (unlimited) → skip the whole logic.
/// - `quota_unit` token mode: delta = `usage.total_tokens`; if the request
///   returned no usage info (`total_tokens == 0`), skip silently.
/// - `quota_unit` call mode: delta = 1.
/// - After accumulating, if `new_used >= quota_limit`, mark `Exhausted`.
/// - Incremental cache update via `ConfigCache::update_small_model_quota`
///   (no full rebuild); durable bump via `UsageWriter::add_small_model_usage`.
///
/// Best-effort: a storage error here is swallowed so that quota bookkeeping
/// can never fail an otherwise-successful request.
async fn accumulate_small_model_quota(
    writer: &dyn UsageWriter,
    cache: &ConfigCache,
    channel_id: i64,
    upstream_model_name: &str,
    usage: &Usage,
) {
    // No cached entry → not a configured small model (or cache not loaded) → skip.
    let Some(state) = cache.get_small_model_quota(channel_id, upstream_model_name).await else {
        return;
    };
    // No limit configured → unlimited → skip the whole accumulation.
    let Some(limit) = state.limit else {
        return;
    };
    // Determine the delta based on quota_unit.
    let delta: i64 = match state.unit {
        Some(QuotaUnit::Token) => {
            // token mode: add total_tokens. If the upstream returned no
            // usage info (total_tokens == 0), skip silently.
            let tokens = usage.total_tokens as i64;
            if tokens == 0 {
                return;
            }
            tokens
        }
        Some(QuotaUnit::Call) => {
            // call mode: count one request.
            1
        }
        None => {
            // No unit configured → cannot determine how to accumulate → skip.
            return;
        }
    };
    let new_used = state.used.saturating_add(delta);
    let status = if new_used >= limit {
        QuotaStatus::Exhausted
    } else {
        QuotaStatus::Available
    };
    // Incremental cache update (no full rebuild).
    cache
        .update_small_model_quota(channel_id, upstream_model_name, new_used, status.clone())
        .await;
    // Durable bump. Best-effort: don't let a storage hiccup fail the request.
    if let Err(e) = writer
        .add_small_model_usage(
            channel_id,
            upstream_model_name,
            delta,
            status.as_db_str(),
        )
        .await
    {
        tracing::debug!(
            "small-model quota bump failed for (channel={}, upstream={}): {}",
            channel_id,
            upstream_model_name,
            e
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::{CacheData, CacheLoader, QuotaState, QuotaWindow};
    use crate::normalizer::Normalizer;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    /// In-memory mock that records every call.
    struct MockWriter {
        events: Mutex<Vec<MockEvent>>,
    }

    #[derive(Debug, Clone, PartialEq)]
    enum MockEvent {
        LogUsage {
            user_id: i64,
            token_id: i64,
            channel_id: i64,
            key_id: i64,
            model_id: i64,
            usage: Usage,
            quota_cost: i64,
            request_type: String,
            status: String,
            error: Option<String>,
        },
        AddKeyUsage { key_id: i64, tokens: u64 },
        AddSmallModelUsage {
            channel_id: i64,
            upstream_model_name: String,
            delta: i64,
            quota_status: String,
        },
    }

    impl MockWriter {
        fn new() -> Self {
            Self {
                events: Mutex::new(Vec::new()),
            }
        }
        fn events(&self) -> Vec<MockEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl UsageWriter for MockWriter {
        async fn log_usage(
            &self,
            user_id: i64,
            token_id: i64,
            channel_id: i64,
            key_id: i64,
            model_id: i64,
            usage: &Usage,
            quota_cost: i64,
            request_type: &str,
            status: &str,
            error: Option<&str>,
        ) -> ProxyResult<()> {
            self.events.lock().unwrap().push(MockEvent::LogUsage {
                user_id,
                token_id,
                channel_id,
                key_id,
                model_id,
                usage: usage.clone(),
                quota_cost,
                request_type: request_type.to_string(),
                status: status.to_string(),
                error: error.map(|s| s.to_string()),
            });
            Ok(())
        }
        async fn add_key_usage(&self, key_id: i64, tokens: u64) -> ProxyResult<()> {
            self.events
                .lock()
                .unwrap()
                .push(MockEvent::AddKeyUsage { key_id, tokens });
            Ok(())
        }
        async fn add_small_model_usage(
            &self,
            channel_id: i64,
            upstream_model_name: &str,
            delta: i64,
            quota_status: &str,
        ) -> ProxyResult<()> {
            self.events
                .lock()
                .unwrap()
                .push(MockEvent::AddSmallModelUsage {
                    channel_id,
                    upstream_model_name: upstream_model_name.to_string(),
                    delta,
                    quota_status: quota_status.to_string(),
                });
            Ok(())
        }
        async fn log_request(
            &self,
            _request_id: &str,
            _client_ip: Option<&str>,
            _method: &str,
            _path: &str,
            _client_model: Option<&str>,
            _normalized_model: Option<&str>,
            _channel_name: Option<&str>,
            _key_label: Option<&str>,
            _attempted_keys: Option<&str>,
            _upstream_status: Option<i64>,
            _response_status: i64,
            _duration_ms: i64,
            _stream: bool,
            _error_message: Option<&str>,
            _user_id: Option<i64>,
            _token_id: Option<i64>,
            _quota_cost: i64,
        ) -> ProxyResult<()> {
            Ok(())
        }
    }

    /// A `CacheLoader` that returns a fixed snapshot — used to pre-seed a
    /// `ConfigCache` with small-model quota entries in tests.
    struct StaticLoader {
        data: CacheData,
        alias: HashMap<String, (i64, String)>,
    }

    #[async_trait]
    impl CacheLoader for StaticLoader {
        async fn load_all(&self) -> ProxyResult<CacheData> {
            Ok(self.data.clone())
        }
        async fn load_alias_mapping(&self) -> ProxyResult<HashMap<String, (i64, String)>> {
            Ok(self.alias.clone())
        }
    }

    /// Build a `ConfigCache` pre-loaded with one small-model quota entry at
    /// `(channel_id=100, upstream="up-x")` described by `state`.
    async fn make_cache_with_quota(state: QuotaState) -> ConfigCache {
        let mut data = CacheData::default();
        data.small_model_quota
            .insert((100, "up-x".to_string()), state);
        let loader = StaticLoader {
            data,
            alias: HashMap::new(),
        };
        let cache = ConfigCache::new(Arc::new(Normalizer::new()));
        cache.get(&loader).await.unwrap();
        cache
    }

    /// An empty `ConfigCache` (no snapshot loaded) — small-model logic
    /// should be a no-op against this.
    fn empty_cache() -> ConfigCache {
        ConfigCache::new(Arc::new(Normalizer::new()))
    }

    fn usage(p: u64, c: u64) -> Usage {
        Usage {
            prompt_tokens: p,
            completion_tokens: c,
            total_tokens: p + c,
        }
    }

    #[tokio::test]
    async fn test_track_success_logs_and_bumps_counters() {
        let writer = MockWriter::new();
        let health = HealthManager::new();
        let cache = empty_cache();
        let u = usage(100, 50);

        Tracker::track_success(
            &writer, &health, &cache,
            /*user*/ 1, /*token*/ 10, /*channel*/ 100, /*key*/ 1000, /*model*/ 7,
            /*upstream*/ "up-x", &u, /*quota_cost*/ 30, "chat",
        )
        .await
        .unwrap();

        let events = writer.events();
        // LogUsage + AddKeyUsage only — empty cache means no small-model bump.
        assert_eq!(events.len(), 2);
        match &events[0] {
            MockEvent::LogUsage {
                user_id, token_id, channel_id, key_id, model_id, usage, quota_cost,
                request_type, status, error,
            } => {
                assert_eq!(*user_id, 1);
                assert_eq!(*token_id, 10);
                assert_eq!(*channel_id, 100);
                assert_eq!(*key_id, 1000);
                assert_eq!(*model_id, 7);
                assert_eq!(*usage, u);
                assert_eq!(*quota_cost, 30);
                assert_eq!(request_type, "chat");
                assert_eq!(status, "success");
                assert!(error.is_none());
            }
            other => panic!("unexpected event: {:?}", other),
        }
        match &events[1] {
            MockEvent::AddKeyUsage { key_id, tokens } => {
                assert_eq!(*key_id, 1000);
                assert_eq!(*tokens, 150);
            }
            other => panic!("unexpected event: {:?}", other),
        }

        // in-memory health counter also bumped
        let s = health.get_state(1000).await.unwrap();
        assert_eq!(s.used_quota_this_period, 150);
    }

    #[tokio::test]
    async fn test_track_failure_logs_without_bumping_counters() {
        let writer = MockWriter::new();
        let health = HealthManager::new();

        Tracker::track_failure(
            &writer,
            1, 10, 100, 1000, 7,
            "chat", "upstream 503: gateway error",
        )
        .await
        .unwrap();

        let events = writer.events();
        assert_eq!(events.len(), 1, "no AddKeyUsage should fire on failure");
        match &events[0] {
            MockEvent::LogUsage {
                status, error, quota_cost, usage, ..
            } => {
                assert_eq!(status, "failed");
                assert_eq!(error.as_deref(), Some("upstream 503: gateway error"));
                assert_eq!(*quota_cost, 0);
                assert_eq!(usage.total_tokens, 0);
            }
            other => panic!("unexpected event: {:?}", other),
        }

        // health counter must NOT have been bumped
        assert!(health.get_state(1000).await.is_none());
    }

    // ===== small-model quota accumulation tests =====

    #[tokio::test]
    async fn test_track_success_accumulates_token_quota() {
        let writer = MockWriter::new();
        let health = HealthManager::new();
        let cache = make_cache_with_quota(QuotaState {
            limit: Some(1_000_000),
            unit: Some(QuotaUnit::Token),
            window: Some(QuotaWindow::Month),
            used: 400,
            last_reset_at: None,
            status: QuotaStatus::Available,
        })
        .await;
        let u = usage(100, 50); // total_tokens = 150

        Tracker::track_success(
            &writer, &health, &cache,
            1, 10, /*channel*/ 100, 1000, 7,
            /*upstream*/ "up-x", &u, 30, "chat",
        )
        .await
        .unwrap();

        // The third event is the small-model bump: delta=150, still available.
        let events = writer.events();
        assert_eq!(events.len(), 3);
        match &events[2] {
            MockEvent::AddSmallModelUsage {
                channel_id, upstream_model_name, delta, quota_status,
            } => {
                assert_eq!(*channel_id, 100);
                assert_eq!(upstream_model_name, "up-x");
                assert_eq!(*delta, 150);
                assert_eq!(quota_status, "available");
            }
            other => panic!("unexpected event: {:?}", other),
        }

        // Cache was incrementally updated (used 400 + 150 = 550).
        let q = cache.get_small_model_quota(100, "up-x").await.unwrap();
        assert_eq!(q.used, 550);
        assert_eq!(q.status, QuotaStatus::Available);
    }

    #[tokio::test]
    async fn test_track_success_accumulates_call_quota() {
        let writer = MockWriter::new();
        let health = HealthManager::new();
        let cache = make_cache_with_quota(QuotaState {
            limit: Some(100),
            unit: Some(QuotaUnit::Call),
            window: Some(QuotaWindow::Day),
            used: 3,
            last_reset_at: None,
            status: QuotaStatus::Available,
        })
        .await;
        // Even with zero tokens, call mode counts one request.
        let u = usage(0, 0);

        Tracker::track_success(
            &writer, &health, &cache,
            1, 10, 100, 1000, 7, "up-x", &u, 0, "chat",
        )
        .await
        .unwrap();

        let events = writer.events();
        assert_eq!(events.len(), 3);
        match &events[2] {
            MockEvent::AddSmallModelUsage { delta, quota_status, .. } => {
                assert_eq!(*delta, 1);
                assert_eq!(quota_status, "available");
            }
            other => panic!("unexpected event: {:?}", other),
        }
        let q = cache.get_small_model_quota(100, "up-x").await.unwrap();
        assert_eq!(q.used, 4);
        assert_eq!(q.status, QuotaStatus::Available);
    }

    #[tokio::test]
    async fn test_track_success_marks_exhausted_at_limit() {
        let writer = MockWriter::new();
        let health = HealthManager::new();
        let cache = make_cache_with_quota(QuotaState {
            limit: Some(500),
            unit: Some(QuotaUnit::Token),
            window: Some(QuotaWindow::Total),
            used: 400,
            last_reset_at: None,
            status: QuotaStatus::Available,
        })
        .await;
        let u = usage(200, 100); // total_tokens = 300 → 400 + 300 = 700 >= 500

        Tracker::track_success(
            &writer, &health, &cache,
            1, 10, 100, 1000, 7, "up-x", &u, 30, "chat",
        )
        .await
        .unwrap();

        let events = writer.events();
        assert_eq!(events.len(), 3);
        match &events[2] {
            MockEvent::AddSmallModelUsage { delta, quota_status, .. } => {
                assert_eq!(*delta, 300);
                assert_eq!(quota_status, "exhausted");
            }
            other => panic!("unexpected event: {:?}", other),
        }
        let q = cache.get_small_model_quota(100, "up-x").await.unwrap();
        assert_eq!(q.used, 700);
        assert_eq!(q.status, QuotaStatus::Exhausted);
    }

    #[tokio::test]
    async fn test_track_success_skips_when_unlimited() {
        let writer = MockWriter::new();
        let health = HealthManager::new();
        // quota_limit = None → unlimited → skip.
        let cache = make_cache_with_quota(QuotaState {
            limit: None,
            unit: Some(QuotaUnit::Token),
            window: None,
            used: 0,
            last_reset_at: None,
            status: QuotaStatus::Available,
        })
        .await;
        let u = usage(100, 50);

        Tracker::track_success(
            &writer, &health, &cache,
            1, 10, 100, 1000, 7, "up-x", &u, 30, "chat",
        )
        .await
        .unwrap();

        // No AddSmallModelUsage event — only LogUsage + AddKeyUsage.
        let events = writer.events();
        assert_eq!(events.len(), 2);
        // Cache state untouched.
        let q = cache.get_small_model_quota(100, "up-x").await.unwrap();
        assert_eq!(q.used, 0);
        assert_eq!(q.status, QuotaStatus::Available);
    }

    #[tokio::test]
    async fn test_track_success_skips_token_mode_zero_usage() {
        let writer = MockWriter::new();
        let health = HealthManager::new();
        let cache = make_cache_with_quota(QuotaState {
            limit: Some(1_000_000),
            unit: Some(QuotaUnit::Token),
            window: Some(QuotaWindow::Month),
            used: 100,
            last_reset_at: None,
            status: QuotaStatus::Available,
        })
        .await;
        // No usage info from upstream → total_tokens == 0 → skip silently.
        let u = usage(0, 0);

        Tracker::track_success(
            &writer, &health, &cache,
            1, 10, 100, 1000, 7, "up-x", &u, 0, "chat",
        )
        .await
        .unwrap();

        let events = writer.events();
        assert_eq!(events.len(), 2, "no small-model bump when total_tokens == 0");
        let q = cache.get_small_model_quota(100, "up-x").await.unwrap();
        assert_eq!(q.used, 100, "used unchanged");
    }

    #[tokio::test]
    async fn test_track_success_skips_unknown_upstream() {
        let writer = MockWriter::new();
        let health = HealthManager::new();
        // Cache has an entry for "up-x", but the request used "other-up".
        let cache = make_cache_with_quota(QuotaState {
            limit: Some(1_000_000),
            unit: Some(QuotaUnit::Token),
            window: Some(QuotaWindow::Month),
            used: 0,
            last_reset_at: None,
            status: QuotaStatus::Available,
        })
        .await;
        let u = usage(100, 50);

        Tracker::track_success(
            &writer, &health, &cache,
            1, 10, 100, 1000, 7,
            /*upstream*/ "other-up", &u, 30, "chat",
        )
        .await
        .unwrap();

        // No entry for "other-up" → skip.
        let events = writer.events();
        assert_eq!(events.len(), 2);
    }
}
