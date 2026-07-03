//! Runtime health manager for upstream keys.
//!
//! Maintains per-key transient state (cooldown window, failure count,
//! quota usage this period) and persists critical state changes (disabled,
//! recovered) to the DB so that they survive server restarts.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

use chennix_common::KeyStatus;
use chennix_storage::channels::DiscoveredModelRepo;
use chennix_storage::keys::KeyRepo;
use chrono::{DateTime, Datelike, Duration, NaiveDateTime, TimeZone, Utc};
use rusqlite::Connection;
use tracing;

use crate::cache::{ConfigCache, QuotaStatus, QuotaWindow};

/// Shared DB connection type used for persistence.
pub type SharedDb = Arc<Mutex<Connection>>;

/// Transient per-key state kept in memory.
#[derive(Debug, Clone)]
pub struct KeyRuntimeState {
    pub key_id: i64,
    pub status: KeyStatus,
    pub cooldown_until: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
    pub used_quota_this_period: u64,
}

impl KeyRuntimeState {
    fn new(key_id: i64) -> Self {
        Self {
            key_id,
            status: KeyStatus::Active,
            cooldown_until: None,
            consecutive_failures: 0,
            used_quota_this_period: 0,
        }
    }
}

/// Transient per-small-model runtime state kept in memory.
///
/// Keyed by `(channel_id, upstream_model_name)` — equivalent to
/// `discovered_models.(channel_id, raw_model_name)`. Mirrors
/// `KeyRuntimeState` but tracks small-model quota windows instead of key
/// cooldowns. The authoritative snapshot is loaded from the DB at startup
/// via [`HealthManager::load_small_model_quota_from_db`]; `check_recoveries`
/// keeps it fresh by resetting rolled-over windows and persisting back.
#[derive(Debug, Clone)]
pub struct SmallModelState {
    pub channel_id: i64,
    pub upstream: String,
    /// `'day'` / `'month'` / `'total'`; `None` when the column is NULL or
    /// unparsable — treated as no auto-reset.
    pub window: Option<QuotaWindow>,
    /// `None` means no limit (always available).
    pub limit: Option<i64>,
    pub used: i64,
    pub last_reset_at: Option<DateTime<Utc>>,
    pub status: QuotaStatus,
}

pub struct HealthManager {
    states: Arc<RwLock<HashMap<i64, KeyRuntimeState>>>,
    small_model_states: Arc<RwLock<HashMap<(i64, String), SmallModelState>>>,
    db: Option<SharedDb>,
    /// When present, small-model quota resets performed in `check_recoveries`
    /// are propagated incrementally (no full cache reload).
    cache: Option<Arc<ConfigCache>>,
}

/// Hard cap on a single cooldown window: 30 minutes.
const MAX_COOLDOWN_SECS: i64 = 1800;

impl HealthManager {
    /// Create a new `HealthManager` without DB persistence.
    /// Mainly useful for tests.
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            small_model_states: Arc::new(RwLock::new(HashMap::new())),
            db: None,
            cache: None,
        }
    }

    /// Create a new `HealthManager` with DB persistence.
    /// On construction this does *not* load state; call
    /// [`load_disabled_from_db`](Self::load_disabled_from_db) afterwards
    /// to restore disabled keys from the previous run.
    pub fn with_db(db: SharedDb) -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            small_model_states: Arc::new(RwLock::new(HashMap::new())),
            db: Some(db),
            cache: None,
        }
    }

    /// Create a new `HealthManager` with DB persistence and a `ConfigCache`
    /// reference. When the cache is present, small-model quota resets done
    /// in `check_recoveries` are propagated incrementally via
    /// [`ConfigCache::update_small_model_quota`] (no full cache reload).
    pub fn with_db_and_cache(db: SharedDb, cache: Arc<ConfigCache>) -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            small_model_states: Arc::new(RwLock::new(HashMap::new())),
            db: Some(db),
            cache: Some(cache),
        }
    }

    /// Load all `disabled` key IDs from the DB and mark them in memory.
    /// Should be called once during startup after the DB is initialised.
    pub async fn load_disabled_from_db(&self) {
        let Some(db) = &self.db else { return };
        let ids = {
            let conn = db.lock().await;
            let repo = KeyRepo::new(&conn);
            match repo.get_disabled_key_ids() {
                Ok(ids) => ids,
                Err(e) => {
                    tracing::warn!("failed to load disabled keys from DB: {}", e);
                    return;
                }
            }
        };
        if ids.is_empty() {
            return;
        }
        let mut map = self.states.write().await;
        for id in ids {
            let state = map.entry(id).or_insert_with(|| KeyRuntimeState::new(id));
            state.status = KeyStatus::Disabled;
        }
        tracing::info!("restored {} disabled keys from DB", map.values().filter(|s| s.status == KeyStatus::Disabled).count());
    }

    /// True iff the key is `Active` and not currently in a cooldown window.
    pub async fn is_available(&self, key_id: i64) -> bool {
        let map = self.states.read().await;
        Self::check_available(&map, key_id)
    }

    /// Non-async variant of `is_available` for use inside synchronous
    /// contexts (e.g. the `Fn(i64) -> bool` predicate passed to
    /// `Router::route`). Uses `try_read`: if the lock is contended it
    /// returns `true` (optimistic) so the caller can fall through to the
    /// authoritative async check in the executor's per-key loop.
    pub fn try_is_available(&self, key_id: i64) -> bool {
        match self.states.try_read() {
            Ok(map) => Self::check_available(&map, key_id),
            Err(_) => true, // optimistic — async path will re-check
        }
    }

    /// Shared implementation for both async + sync variants.
    ///
    /// 可用性判断逻辑：
    /// - `Active` → 可用（除非有未过期的 cooldown_until，防御性检查）
    /// - `Cooldown` + `cooldown_until > now` → 不可用（冷却期内）
    /// - `Cooldown` + `cooldown_until <= now` → **可用**（冷却到期，零延迟恢复）
    /// - `Disabled` / `QuotaExhausted` → 不可用
    ///
    /// 关键：`Cooldown` 到期后不依赖 `check_recoveries` 改 status 才恢复——
    /// `is_available` 内联检查 `cooldown_until`，确保冷却到期立即可用。
    /// 后台 `check_recoveries` 只负责重置 `consecutive_failures`（退避窗口）
    /// 和 persist status 到 DB，不在可用性关键路径上。
    fn check_available(map: &HashMap<i64, KeyRuntimeState>, key_id: i64) -> bool {
        match map.get(&key_id) {
            None => true, // unseen keys are considered available
            Some(state) => {
                // 先检查 cooldown_until（对所有状态通用，但主要针对 Cooldown）
                if let Some(until) = state.cooldown_until {
                    if until > Utc::now() {
                        // 冷却未到期：无论 status 如何，都不可用
                        return false;
                    }
                    // 冷却已到期：继续检查 status
                }
                // 冷却已到期或无冷却：按 status 判断
                // Cooldown 到期后视为可用（零延迟恢复，对齐 new-api 行为）
                state.status.is_available() || state.status == KeyStatus::Cooldown
            }
        }
    }

    /// Apply exponential backoff: the next cooldown lasts `2^n` seconds
    /// (capped at 1800s = 30min), where `n` is the updated failure count.
    /// Each successive `mark_cooldown` call within the failure streak
    /// doubles the window.
    ///
    /// Cooldown is transient — it is **not** persisted to the DB.
    pub async fn mark_cooldown(&self, key_id: i64) {
        let mut map = self.states.write().await;
        let state = map.entry(key_id).or_insert_with(|| KeyRuntimeState::new(key_id));
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        let n = state.consecutive_failures;
        let secs = (1i64 << n.min(31)).min(MAX_COOLDOWN_SECS);
        state.cooldown_until = Some(Utc::now() + Duration::seconds(secs));
        state.status = KeyStatus::Cooldown;
    }

    /// Permanently disable a key in the runtime layer (e.g. on 401/403).
    /// Also persists the `disabled` status to the DB so it survives restarts.
    /// `restore_disabled` is the only way back.
    pub async fn mark_disabled(&self, key_id: i64) {
        {
            let mut map = self.states.write().await;
            let state = map.entry(key_id).or_insert_with(|| KeyRuntimeState::new(key_id));
            state.status = KeyStatus::Disabled;
            state.cooldown_until = None;
            // We keep consecutive_failures so a later restore_disabled+mark_cooldown
            // chain still has history, though restore_disabled resets it.
        }
        // Persist to DB (outside the in-memory lock to avoid holding it across I/O).
        self.persist_status(key_id, KeyStatus::Disabled).await;
    }

    /// Record token usage against the key's current billing period.
    /// Used by the tracker to keep the in-memory quota counter fresh so
    /// that `Router::route` can sort by remaining-ratio without a DB round
    /// trip on every request.
    pub async fn add_usage(&self, key_id: i64, tokens: u64) {
        let mut map = self.states.write().await;
        let state = map.entry(key_id).or_insert_with(|| KeyRuntimeState::new(key_id));
        state.used_quota_this_period = state.used_quota_this_period.saturating_add(tokens);
    }

    /// Clear expired cooldowns, returning affected keys to `Active`.
    /// Also persists recovered keys to the DB.
    ///
    /// Called only by the background loop in `main.rs` (every 10s) — NOT
    /// on the per-request path. `is_available` checks `cooldown_until`
    /// inline so availability is unaffected; this loop only resets
    /// `consecutive_failures` (backoff window) and rolls over small-model
    /// quota windows.
    pub async fn check_recoveries(&self) {
        let now = Utc::now();
        let mut recovered_ids: Vec<i64> = Vec::new();
        {
            let mut map = self.states.write().await;
            for state in map.values_mut() {
                if state.status == KeyStatus::Cooldown {
                    if let Some(until) = state.cooldown_until {
                        if until <= now {
                            state.status = KeyStatus::Active;
                            state.cooldown_until = None;
                            state.consecutive_failures = 0;
                            recovered_ids.push(state.key_id);
                        }
                    }
                }
            }
        }
        // Persist recovered keys to DB as active.
        for id in recovered_ids {
            self.persist_status(id, KeyStatus::Active).await;
        }

        // Small-model quota period resets (day/month windows roll over).
        self.check_small_model_quota_resets(now).await;
    }

    /// Manually restore keys that were `Disabled` (typically by an admin
    /// action or after a quota reset). Also persists `active` to the DB.
    /// Resets failure counters too.
    pub async fn restore_disabled(&self, key_ids: Vec<i64>) {
        {
            let mut map = self.states.write().await;
            for id in &key_ids {
                let state = map.entry(*id).or_insert_with(|| KeyRuntimeState::new(*id));
                state.status = KeyStatus::Active;
                state.cooldown_until = None;
                state.consecutive_failures = 0;
            }
        }
        for id in key_ids {
            self.persist_status(id, KeyStatus::Active).await;
        }
    }

    /// Re-sync a single key's status from the DB into memory.
    /// Called when an admin API changes a key's status so that the
    /// in-memory view matches the DB.
    pub async fn sync_key_from_db(&self, key_id: i64, new_status: KeyStatus) {
        let mut map = self.states.write().await;
        let state = map.entry(key_id).or_insert_with(|| KeyRuntimeState::new(key_id));
        state.status = new_status;
        if new_status == KeyStatus::Active {
            state.cooldown_until = None;
            state.consecutive_failures = 0;
        }
    }

    /// Read-only snapshot of a key's runtime state (for diagnostics /
    /// admin endpoints).
    pub async fn get_state(&self, key_id: i64) -> Option<KeyRuntimeState> {
        let map = self.states.read().await;
        map.get(&key_id).cloned()
    }

    // ------------------------------------------------------------------
    // Small-model quota
    // ------------------------------------------------------------------

    /// Load all small-model quota rows from the DB into the in-memory map.
    /// Should be called once during startup after the DB is initialised
    /// (mirrors [`load_disabled_from_db`](Self::load_disabled_from_db)).
    /// Safe to call again to refresh the in-memory snapshot.
    pub async fn load_small_model_quota_from_db(&self) {
        let Some(db) = &self.db else { return };
        let rows = {
            let conn = db.lock().await;
            let repo = DiscoveredModelRepo::new(&conn);
            match repo.list_discovered_models() {
                Ok(rows) => rows,
                Err(e) => {
                    tracing::warn!("failed to load discovered models for quota: {}", e);
                    return;
                }
            }
        };
        let mut map = self.small_model_states.write().await;
        for row in rows {
            let window = row
                .quota_window
                .as_deref()
                .and_then(QuotaWindow::from_db_str);
            let status = QuotaStatus::from_db_str(&row.quota_status);
            let last_reset_at = row
                .last_reset_at
                .as_deref()
                .and_then(Self::parse_db_datetime);
            map.insert(
                (row.channel_id, row.raw_model_name.clone()),
                SmallModelState {
                    channel_id: row.channel_id,
                    upstream: row.raw_model_name,
                    window,
                    limit: row.quota_limit,
                    used: row.used_quota,
                    last_reset_at,
                    status,
                },
            );
        }
        tracing::info!("loaded {} small-model quota entries from DB", map.len());
    }

    /// Synchronous small-model availability check, designed to be called
    /// from a Router `quota_filter` closure (Task 3) — mirrors
    /// [`try_is_available`](Self::try_is_available):
    /// - entry not in map (unseen / no quota configured) → `true`
    /// - `quota_limit` is `None` (no limit) → `true`
    /// - `quota_status` is `Available` → `true`
    /// - `quota_status` is `Exhausted` → `false`
    ///
    /// Uses `try_read`: if the internal lock is contended it returns `true`
    /// (optimistic) so the caller can fall through to an authoritative
    /// async re-check if needed.
    pub fn is_small_model_available(&self, channel_id: i64, upstream: &str) -> bool {
        match self.small_model_states.try_read() {
            Ok(map) => match map.get(&(channel_id, upstream.to_string())) {
                None => true,
                Some(state) => {
                    if state.limit.is_none() {
                        return true;
                    }
                    state.status == QuotaStatus::Available
                }
            },
            Err(_) => true, // optimistic — async path can re-check
        }
    }

    // ------------------------------------------------------------------
    // Internal helpers
    // ------------------------------------------------------------------

    /// Write a key's status to the DB. Silently logs a warning on failure
    /// (the in-memory state is the source of truth at runtime).
    async fn persist_status(&self, key_id: i64, status: KeyStatus) {
        let Some(db) = &self.db else { return };
        let conn = db.lock().await;
        let repo = KeyRepo::new(&conn);
        if let Err(e) = repo.update_key_status(key_id, status) {
            tracing::warn!(
                "failed to persist key {} status {:?} to DB: {}",
                key_id,
                status,
                e
            );
        }
    }

    /// Reset small-model quotas whose `day`/`month` window has rolled over
    /// since `last_reset_at`. For each reset: updates the in-memory state,
    /// persists via [`DiscoveredModelRepo::reset_discovered_model_quota`]
    /// (when a DB is attached), and incrementally updates the
    /// [`ConfigCache`] (when attached). `total` windows are never reset.
    async fn check_small_model_quota_resets(&self, now: DateTime<Utc>) {
        let mut reset_entries: Vec<(i64, String)> = Vec::new();
        {
            let mut map = self.small_model_states.write().await;
            for state in map.values_mut() {
                let Some(window) = &state.window else { continue };
                if !Self::period_rolled(window, state.last_reset_at, now) {
                    continue;
                }
                state.used = 0;
                state.status = QuotaStatus::Available;
                state.last_reset_at = Some(now);
                reset_entries.push((state.channel_id, state.upstream.clone()));
            }
        }
        for (channel_id, upstream) in reset_entries {
            self.persist_small_model_reset(channel_id, &upstream).await;
        }
    }

    /// Persist a single small-model quota reset to the DB (when attached)
    /// and propagate it to the `ConfigCache` (when attached).
    async fn persist_small_model_reset(&self, channel_id: i64, upstream: &str) {
        if let Some(db) = &self.db {
            let conn = db.lock().await;
            let repo = DiscoveredModelRepo::new(&conn);
            if let Err(e) = repo.reset_discovered_model_quota(channel_id, upstream) {
                tracing::warn!(
                    "failed to persist small-model quota reset (channel={}, upstream={}): {}",
                    channel_id,
                    upstream,
                    e
                );
            }
        }
        if let Some(cache) = &self.cache {
            cache
                .update_small_model_quota(channel_id, upstream, 0, QuotaStatus::Available)
                .await;
        }
    }

    /// True iff the quota window has rolled over since `last_reset_at`.
    /// `None` `last_reset_at` is treated as needing a reset (establishes a
    /// baseline timestamp on first check).
    fn period_rolled(
        window: &QuotaWindow,
        last_reset_at: Option<DateTime<Utc>>,
        now: DateTime<Utc>,
    ) -> bool {
        match window {
            QuotaWindow::Total => false,
            QuotaWindow::Day => match last_reset_at {
                None => true,
                Some(t) => t.date_naive() != now.date_naive(),
            },
            QuotaWindow::Month => match last_reset_at {
                None => true,
                Some(t) => t.year() != now.year() || t.month() != now.month(),
            },
        }
    }

    /// Parse a SQLite `datetime('now')`-style string (`YYYY-MM-DD HH:MM:SS`,
    /// assumed UTC) into a `DateTime<Utc>`. Falls back to RFC 3339. Returns
    /// `None` on failure.
    fn parse_db_datetime(s: &str) -> Option<DateTime<Utc>> {
        let s = s.trim();
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|ndt| Utc.from_utc_datetime(&ndt))
            .or_else(|| DateTime::parse_from_rfc3339(s).ok().map(|d| d.with_timezone(&Utc)))
    }
}

impl Default for HealthManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mark_cooldown_sets_status_and_window() {
        let h = HealthManager::new();
        assert!(h.is_available(1).await);

        h.mark_cooldown(1).await;
        let s = h.get_state(1).await.unwrap();
        assert_eq!(s.status, KeyStatus::Cooldown);
        assert!(s.cooldown_until.is_some());
        assert_eq!(s.consecutive_failures, 1);
        assert!(!h.is_available(1).await);
    }

    #[tokio::test]
    async fn test_cooldown_expired_zero_latency_recovery() {
        // 关键回归测试：Cooldown 到期后 is_available 必须立即返回 true，
        // 不依赖 check_recoveries 改 status。这是 2.5 修复的基础——
        // 移除每请求 check_recoveries 后，可用性零延迟恢复。
        let h = HealthManager::new();

        // 设置 Cooldown + 过期的 cooldown_until（模拟冷却到期但
        // check_recoveries 还没跑的场景）
        {
            let mut map = h.states.write().await;
            let state = map.entry(1).or_insert_with(|| KeyRuntimeState::new(1));
            state.status = KeyStatus::Cooldown;
            state.cooldown_until = Some(Utc::now() - Duration::seconds(1)); // 1秒前到期
            state.consecutive_failures = 3;
        }

        // 冷却到期 → 立即可用（即使 status 仍是 Cooldown）
        assert!(h.is_available(1).await, "expired cooldown should be available");

        // 同步路径也要验证（Router::route 用 try_is_available）
        assert!(h.try_is_available(1), "expired cooldown should be available (sync)");

        // status 仍是 Cooldown（check_recoveries 才会改成 Active）
        let s = h.get_state(1).await.unwrap();
        assert_eq!(s.status, KeyStatus::Cooldown, "status not yet reset by check_recoveries");
    }

    #[tokio::test]
    async fn test_cooldown_not_yet_expired_unavailable() {
        // 对比测试：Cooldown 未到期 → 不可用
        let h = HealthManager::new();
        h.mark_cooldown(1).await; // 默认 cooldown 至少 2 秒

        // 冷却未到期 → 不可用
        assert!(!h.is_available(1).await, "active cooldown should be unavailable");
        assert!(!h.try_is_available(1), "active cooldown should be unavailable (sync)");
    }

    #[tokio::test]
    async fn test_exponential_backoff_grows_then_caps() {
        let h = HealthManager::new();
        // Track the cooldown window after each failure.
        let mut windows: Vec<i64> = Vec::new();
        for _ in 0..12 {
            h.mark_cooldown(1).await;
            let s = h.get_state(1).await.unwrap();
            let now = Utc::now();
            let secs = s
                .cooldown_until
                .map(|t| (t - now).num_seconds().max(0))
                .unwrap_or(0);
            windows.push(secs);
        }

        // First call: n=1 → 2^1 = 2s
        assert!(windows[0] >= 1 && windows[0] <= 2, "first cooldown: {}", windows[0]);
        // Growth: each subsequent window should be >= the previous one
        for w in windows.windows(2) {
            assert!(
                w[1] >= w[0],
                "expected non-decreasing windows: {} then {}",
                w[0],
                w[1]
            );
        }
        // Cap: 2^31 is huge but we clamp to 1800s
        assert!(
            windows.last().copied().unwrap() <= MAX_COOLDOWN_SECS + 5,
            "cooldown exceeded cap: {:?}",
            windows.last()
        );
    }

    #[tokio::test]
    async fn test_check_recoveries_clears_expired_cooldowns() {
        let h = HealthManager::new();

        // Manually push a state whose cooldown expired.
        {
            let mut map = h.states.write().await;
            map.insert(
                1,
                KeyRuntimeState {
                    key_id: 1,
                    status: KeyStatus::Cooldown,
                    cooldown_until: Some(Utc::now() - Duration::seconds(60)),
                    consecutive_failures: 3,
                    used_quota_this_period: 0,
                },
            );
        }
        // 过期 cooldown 在 check_recoveries 之前就已可用（零延迟恢复，
        // 见 test_cooldown_expired_zero_latency_recovery）。这里不再断言
        // "check_recoveries 之前不可用"——那是旧行为。
        assert!(h.is_available(1).await);

        // check_recoveries 的职责：重置 status 为 Active + 清 cooldown_until
        // + 清 consecutive_failures + persist 到 DB。
        h.check_recoveries().await;
        let s = h.get_state(1).await.unwrap();
        assert_eq!(s.status, KeyStatus::Active);
        assert!(s.cooldown_until.is_none());
        assert_eq!(s.consecutive_failures, 0);
        assert!(h.is_available(1).await);
    }

    #[tokio::test]
    async fn test_is_available_active_unseen_disabled() {
        let h = HealthManager::new();

        // unseen key → available
        assert!(h.is_available(999).await);

        // disabled key → unavailable
        h.mark_disabled(2).await;
        assert!(!h.is_available(2).await);
        let s = h.get_state(2).await.unwrap();
        assert_eq!(s.status, KeyStatus::Disabled);

        // restore_disabled brings it back
        h.restore_disabled(vec![2]).await;
        assert!(h.is_available(2).await);
        let s = h.get_state(2).await.unwrap();
        assert_eq!(s.status, KeyStatus::Active);
        assert_eq!(s.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn test_add_usage_accumulates() {
        let h = HealthManager::new();
        h.add_usage(1, 100).await;
        h.add_usage(1, 50).await;
        let s = h.get_state(1).await.unwrap();
        assert_eq!(s.used_quota_this_period, 150);
    }

    #[tokio::test]
    async fn test_check_recoveries_leaves_active_keys_alone() {
        let h = HealthManager::new();
        h.add_usage(1, 100).await;
        h.check_recoveries().await;
        let s = h.get_state(1).await.unwrap();
        assert_eq!(s.status, KeyStatus::Active);
        assert_eq!(s.used_quota_this_period, 100);
    }

    #[tokio::test]
    async fn test_sync_key_from_db() {
        let h = HealthManager::new();
        h.mark_disabled(5).await;
        assert!(!h.is_available(5).await);

        // Simulate admin re-enabling the key via DB → memory sync.
        h.sync_key_from_db(5, KeyStatus::Active).await;
        assert!(h.is_available(5).await);
    }

    #[tokio::test]
    async fn test_with_db_persists_disabled() {
        use chennix_storage::schema::init_db;

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // Create a channel + key so we have a valid row.
        {
            let ch_repo = chennix_storage::channels::ChannelRepo::new(&conn);
            ch_repo
                .create_channel(
                    "test",
                    &chennix_common::ChannelProvider::OpenaiCompatible,
                    "http://test",
                )
                .unwrap();
            let key_repo = KeyRepo::new(&conn);
            key_repo
                .create_key(1, "sk-test", None, chennix_common::CostTier::Free, 1, None, None, None)
                .unwrap();
        }

        let db: SharedDb = Arc::new(Mutex::new(conn));
        let h = HealthManager::with_db(db.clone());

        // mark_disabled should persist to DB.
        h.mark_disabled(1).await;

        // Verify DB was written.
        {
            let conn = db.lock().await;
            let repo = KeyRepo::new(&conn);
            let key = repo.get_key_by_id(1).unwrap().unwrap();
            assert_eq!(key.status, KeyStatus::Disabled);
        }

        // restore_disabled should set DB back to active.
        h.restore_disabled(vec![1]).await;
        {
            let conn = db.lock().await;
            let repo = KeyRepo::new(&conn);
            let key = repo.get_key_by_id(1).unwrap().unwrap();
            assert_eq!(key.status, KeyStatus::Active);
        }
    }

    #[tokio::test]
    async fn test_load_disabled_from_db_on_startup() {
        use chennix_storage::schema::init_db;

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        {
            let ch_repo = chennix_storage::channels::ChannelRepo::new(&conn);
            ch_repo
                .create_channel(
                    "test",
                    &chennix_common::ChannelProvider::OpenaiCompatible,
                    "http://test",
                )
                .unwrap();
            let key_repo = KeyRepo::new(&conn);
            let id = key_repo
                .create_key(1, "sk-test", None, chennix_common::CostTier::Free, 1, None, None, None)
                .unwrap();
            // Pre-set the key as disabled in DB.
            key_repo.update_key_status(id, KeyStatus::Disabled).unwrap();
        }

        let db: SharedDb = Arc::new(Mutex::new(conn));
        let h = HealthManager::with_db(db.clone());

        // Before loading, key 1 should be "available" (unseen).
        assert!(h.is_available(1).await);

        // Load disabled keys from DB.
        h.load_disabled_from_db().await;

        // Now key 1 should be unavailable.
        assert!(!h.is_available(1).await);
        let s = h.get_state(1).await.unwrap();
        assert_eq!(s.status, KeyStatus::Disabled);
    }

    // ------------------------------------------------------------------
    // Small-model quota reset tests
    // ------------------------------------------------------------------

    async fn put_small_model_state(
        h: &HealthManager,
        channel_id: i64,
        upstream: &str,
        window: Option<QuotaWindow>,
        limit: Option<i64>,
        used: i64,
        last_reset_at: Option<DateTime<Utc>>,
        status: QuotaStatus,
    ) {
        let mut map = h.small_model_states.write().await;
        map.insert(
            (channel_id, upstream.to_string()),
            SmallModelState {
                channel_id,
                upstream: upstream.to_string(),
                window,
                limit,
                used,
                last_reset_at,
                status,
            },
        );
    }

    /// A datetime firmly in last month (avoids month-boundary ambiguity).
    fn last_month_dt() -> DateTime<Utc> {
        let now = Utc::now();
        let (y, m) = if now.month() == 1 {
            (now.year() - 1, 12u32)
        } else {
            (now.year(), now.month() - 1)
        };
        Utc.with_ymd_and_hms(y, m, 15, 0, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn test_small_model_day_reset_triggers_when_yesterday() {
        let h = HealthManager::new();
        put_small_model_state(
            &h,
            1,
            "gpt-4o",
            Some(QuotaWindow::Day),
            Some(1000),
            1000,
            Some(Utc::now() - Duration::days(1)),
            QuotaStatus::Exhausted,
        )
        .await;

        h.check_recoveries().await;

        let map = h.small_model_states.read().await;
        let s = map.get(&(1, "gpt-4o".to_string())).unwrap();
        assert_eq!(s.used, 0);
        assert_eq!(s.status, QuotaStatus::Available);
        assert!(s.last_reset_at.is_some());
    }

    #[tokio::test]
    async fn test_small_model_day_reset_skips_when_today() {
        let h = HealthManager::new();
        put_small_model_state(
            &h,
            1,
            "gpt-4o",
            Some(QuotaWindow::Day),
            Some(1000),
            500,
            Some(Utc::now()),
            QuotaStatus::Available,
        )
        .await;

        h.check_recoveries().await;

        let map = h.small_model_states.read().await;
        let s = map.get(&(1, "gpt-4o".to_string())).unwrap();
        assert_eq!(s.used, 500, "used must be unchanged when last_reset_at is today");
        assert_eq!(s.status, QuotaStatus::Available);
    }

    #[tokio::test]
    async fn test_small_model_month_reset_triggers_when_last_month() {
        let h = HealthManager::new();
        put_small_model_state(
            &h,
            2,
            "claude",
            Some(QuotaWindow::Month),
            Some(1000),
            1000,
            Some(last_month_dt()),
            QuotaStatus::Exhausted,
        )
        .await;

        h.check_recoveries().await;

        let map = h.small_model_states.read().await;
        let s = map.get(&(2, "claude".to_string())).unwrap();
        assert_eq!(s.used, 0);
        assert_eq!(s.status, QuotaStatus::Available);
    }

    #[tokio::test]
    async fn test_small_model_month_reset_skips_when_same_month() {
        let h = HealthManager::new();
        put_small_model_state(
            &h,
            2,
            "claude",
            Some(QuotaWindow::Month),
            Some(1000),
            800,
            Some(Utc::now()),
            QuotaStatus::Available,
        )
        .await;

        h.check_recoveries().await;

        let map = h.small_model_states.read().await;
        let s = map.get(&(2, "claude".to_string())).unwrap();
        assert_eq!(s.used, 800, "used must be unchanged within the same month");
    }

    #[tokio::test]
    async fn test_small_model_total_never_resets() {
        let h = HealthManager::new();
        put_small_model_state(
            &h,
            3,
            "gemini",
            Some(QuotaWindow::Total),
            Some(1000),
            1000,
            Some(Utc::now() - Duration::days(400)),
            QuotaStatus::Exhausted,
        )
        .await;

        h.check_recoveries().await;

        let map = h.small_model_states.read().await;
        let s = map.get(&(3, "gemini".to_string())).unwrap();
        assert_eq!(s.used, 1000, "total window must not auto-reset");
        assert_eq!(s.status, QuotaStatus::Exhausted);
    }

    #[tokio::test]
    async fn test_is_small_model_available_variants() {
        let h = HealthManager::new();

        // unseen (channel_id, upstream) → available
        assert!(h.is_small_model_available(99, "unseen"));

        // quota_limit None (no limit) → available even if Exhausted somehow
        put_small_model_state(
            &h,
            1,
            "a",
            Some(QuotaWindow::Day),
            None,
            0,
            None,
            QuotaStatus::Exhausted,
        )
        .await;
        assert!(h.is_small_model_available(1, "a"));

        // status Available → true
        put_small_model_state(
            &h,
            2,
            "b",
            Some(QuotaWindow::Day),
            Some(1000),
            100,
            Some(Utc::now()),
            QuotaStatus::Available,
        )
        .await;
        assert!(h.is_small_model_available(2, "b"));

        // status Exhausted (with limit) → false
        put_small_model_state(
            &h,
            3,
            "c",
            Some(QuotaWindow::Day),
            Some(1000),
            1000,
            Some(Utc::now()),
            QuotaStatus::Exhausted,
        )
        .await;
        assert!(!h.is_small_model_available(3, "c"));
    }

    #[tokio::test]
    async fn test_small_model_reset_persists_to_db() {
        use chennix_storage::schema::init_db;

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        {
            conn.execute(
                "INSERT INTO channels (id, name, provider, base_url) \
                 VALUES (1, 'ch', 'openai-compatible', 'http://x')",
                [],
            )
            .unwrap();
            let repo = DiscoveredModelRepo::new(&conn);
            repo.upsert_discovered_model(1, "gpt-4o", false, None, None)
                .unwrap();
            repo.update_discovered_model_quota(1, "gpt-4o", Some(1000), Some("token"), Some("day"))
                .unwrap();
            // Simulate yesterday's reset + exhausted quota.
            conn.execute(
                "UPDATE discovered_models \
                 SET used_quota = 1000, quota_status = 'exhausted', \
                     last_reset_at = datetime('now', '-1 day') \
                 WHERE channel_id = 1 AND raw_model_name = 'gpt-4o'",
                [],
            )
            .unwrap();
        }

        let db: SharedDb = Arc::new(Mutex::new(conn));
        let h = HealthManager::with_db(db.clone());
        h.load_small_model_quota_from_db().await;
        h.check_recoveries().await;

        // Verify the DB row was reset.
        {
            let conn = db.lock().await;
            let repo = DiscoveredModelRepo::new(&conn);
            let m = repo.get_discovered_model(1, "gpt-4o").unwrap().unwrap();
            assert_eq!(m.used_quota, 0);
            assert_eq!(m.quota_status, "available");
        }
    }

    #[tokio::test]
    async fn test_small_model_reset_updates_cache() {
        use async_trait::async_trait;
        use chennix_common::ProxyResult;
        use chennix_storage::schema::init_db;

        // Minimal loader that hands the cache an Exhausted entry for (1, "gpt-4o").
        struct StubLoader;
        #[async_trait]
        impl crate::cache::CacheLoader for StubLoader {
            async fn load_all(&self) -> ProxyResult<crate::cache::CacheData> {
                let mut data = crate::cache::CacheData::default();
                let mut sm = HashMap::new();
                sm.insert(
                    (1, "gpt-4o".to_string()),
                    crate::cache::QuotaState {
                        limit: Some(1000),
                        unit: None,
                        window: Some(crate::cache::QuotaWindow::Day),
                        used: 1000,
                        last_reset_at: None,
                        status: crate::cache::QuotaStatus::Exhausted,
                    },
                );
                data.small_model_quota = sm;
                Ok(data)
            }
            async fn load_alias_mapping(
                &self,
            ) -> ProxyResult<HashMap<String, (i64, String)>> {
                Ok(Default::default())
            }
        }

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        {
            conn.execute(
                "INSERT INTO channels (id, name, provider, base_url) \
                 VALUES (1, 'ch', 'openai-compatible', 'http://x')",
                [],
            )
            .unwrap();
            let repo = DiscoveredModelRepo::new(&conn);
            repo.upsert_discovered_model(1, "gpt-4o", false, None, None)
                .unwrap();
            repo.update_discovered_model_quota(1, "gpt-4o", Some(1000), Some("token"), Some("day"))
                .unwrap();
            conn.execute(
                "UPDATE discovered_models \
                 SET used_quota = 1000, quota_status = 'exhausted', \
                     last_reset_at = datetime('now', '-1 day') \
                 WHERE channel_id = 1 AND raw_model_name = 'gpt-4o'",
                [],
            )
            .unwrap();
        }

        let normalizer = Arc::new(crate::normalizer::Normalizer::new());
        let cache = Arc::new(ConfigCache::new(normalizer));
        let loader = StubLoader;
        // Populate the cache with the Exhausted entry.
        let _ = cache.get(&loader).await.unwrap();
        let d = cache.get(&loader).await.unwrap();
        assert_eq!(
            d.small_model_quota_for(1, "gpt-4o").unwrap().status,
            QuotaStatus::Exhausted
        );

        let db: SharedDb = Arc::new(Mutex::new(conn));
        let h = HealthManager::with_db_and_cache(db, cache.clone());
        h.load_small_model_quota_from_db().await;
        h.check_recoveries().await;

        // The reset must have been propagated incrementally to the cache
        // (no reload; the StubLoader still reports Exhausted, so an
        // unmodified cache would still read Exhausted).
        let d = cache.get(&loader).await.unwrap();
        let q = d.small_model_quota_for(1, "gpt-4o").unwrap();
        assert_eq!(q.used, 0);
        assert_eq!(q.status, QuotaStatus::Available);
    }
}
