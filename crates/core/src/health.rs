//! Runtime health manager for upstream keys.
//!
//! Maintains per-key transient state (cooldown window, failure count,
//! quota usage this period) and persists critical state changes (disabled,
//! recovered) to the DB so that they survive server restarts.
//!
//! ## Cooldown granularity
//! Cooldown is tracked **per (key_id, upstream_model_name)**, not per key.
//! This means a key that times out or gets rate-limited on one upstream
//! model is still available for other upstream models it is bound to.
//! For example, if a key fails on `deepseek-chat`, it is still tried for
//! `gemini-2.0-flash` — only the `(key, "deepseek-chat")` pair enters the
//! cooldown window. Key-level `status` (Active/Disabled) is reserved for
//! permanent failures (401/403) that affect the whole key.

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

/// Per-(key, upstream_model) cooldown state. Tracked inside
/// [`KeyRuntimeState::cooldowns`] keyed by `upstream_model_name`.
#[derive(Debug, Clone, Default)]
pub struct CooldownEntry {
    pub cooldown_until: Option<DateTime<Utc>>,
    pub consecutive_failures: u32,
}

/// Transient per-key state kept in memory.
///
/// `status` reflects only persistent key-level state (Active/Disabled).
/// Transient cooldowns live in `cooldowns`, keyed by upstream model name,
/// so a key cooling down on one model is still available for others.
#[derive(Debug, Clone)]
pub struct KeyRuntimeState {
    pub key_id: i64,
    pub status: KeyStatus,
    /// Per-(key, upstream_model) cooldown state. Empty when the key has
    /// no active cooldowns on any upstream model. Entries are cleared by
    /// [`check_recoveries`](HealthManager::check_recoveries) once they
    /// expire (memory hygiene + fresh backoff on next failure).
    pub cooldowns: HashMap<String, CooldownEntry>,
    pub used_quota_this_period: u64,
}

impl KeyRuntimeState {
    fn new(key_id: i64) -> Self {
        Self {
            key_id,
            status: KeyStatus::Active,
            cooldowns: HashMap::new(),
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

    /// True iff the key is available for the given upstream model:
    /// key-level `status` is not Disabled/QuotaExhausted AND there is no
    /// active per-(key, upstream_model) cooldown window.
    pub async fn is_available(&self, key_id: i64, upstream_model: &str) -> bool {
        let map = self.states.read().await;
        Self::check_available(&map, key_id, upstream_model)
    }

    /// Non-async variant of `is_available` for use inside synchronous
    /// contexts (e.g. the `Fn(i64, &str) -> bool` predicate passed to
    /// `Router::route`). Uses `try_read`: if the lock is contended it
    /// returns `true` (optimistic) so the caller can fall through to the
    /// authoritative async check in the executor's per-key loop.
    pub fn try_is_available(&self, key_id: i64, upstream_model: &str) -> bool {
        match self.states.try_read() {
            Ok(map) => Self::check_available(&map, key_id, upstream_model),
            Err(_) => true, // optimistic — async path will re-check
        }
    }

    /// Shared implementation for both async + sync variants.
    ///
    /// 可用性判断逻辑（per-(key, upstream_model) 粒度）：
    /// - key.status 为 `Disabled` / `QuotaExhausted` → 不可用（key 本身坏了）
    /// - key.status 为 `Active` 或 `Cooldown`（legacy） → 检查 per-model cooldown
    /// - `cooldowns[upstream_model].cooldown_until > now` → 不可用（该模型冷却中）
    /// - 否则 → 可用
    ///
    /// 关键：cooldown 到期后 `is_available` 内联检查立即返回 true，
    /// 不依赖 `check_recoveries` 改状态。后台 `check_recoveries` 只负责
    /// 清理过期的 cooldown 条目（内存回收 + 重置退避计数器），不在
    /// 可用性关键路径上。
    fn check_available(
        map: &HashMap<i64, KeyRuntimeState>,
        key_id: i64,
        upstream_model: &str,
    ) -> bool {
        match map.get(&key_id) {
            None => true, // unseen keys are considered available
            Some(state) => {
                // Key-level status check.
                // `|| status == Cooldown` 保留是为了兼容 legacy DB 行
                // （旧版本可能将 status 持久化为 Cooldown）；新代码运行时
                // 永远不会设置 status=Cooldown，cooldown 完全由 `cooldowns`
                // map 跟踪。
                if !(state.status.is_available() || state.status == KeyStatus::Cooldown) {
                    return false;
                }
                // Per-(key, upstream_model) cooldown check.
                if let Some(entry) = state.cooldowns.get(upstream_model) {
                    if let Some(until) = entry.cooldown_until {
                        if until > Utc::now() {
                            return false;
                        }
                    }
                }
                true
            }
        }
    }

    /// Apply exponential backoff for the (key_id, upstream_model) pair:
    /// the next cooldown lasts `2^n` seconds (capped at 1800s = 30min),
    /// where `n` is the updated failure count for this specific pair.
    /// Each successive `mark_cooldown` call within the failure streak
    /// doubles the window.
    ///
    /// **Does NOT change `key.status`** — the key remains Active for other
    /// upstream models. Cooldown is transient and is **not** persisted to
    /// the DB.
    pub async fn mark_cooldown(&self, key_id: i64, upstream_model: &str) {
        let mut map = self.states.write().await;
        let state = map.entry(key_id).or_insert_with(|| KeyRuntimeState::new(key_id));
        let entry = state
            .cooldowns
            .entry(upstream_model.to_string())
            .or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        let n = entry.consecutive_failures;
        let secs = (1i64 << n.min(31)).min(MAX_COOLDOWN_SECS);
        entry.cooldown_until = Some(Utc::now() + Duration::seconds(secs));
        // state.status 不变 — cooldown 是 per-(key, model)，不影响其他模型
    }

    /// Permanently disable a key in the runtime layer (e.g. on 401/403).
    /// Also persists the `disabled` status to the DB so it survives restarts.
    /// `restore_disabled` is the only way back.
    pub async fn mark_disabled(&self, key_id: i64) {
        {
            let mut map = self.states.write().await;
            let state = map.entry(key_id).or_insert_with(|| KeyRuntimeState::new(key_id));
            state.status = KeyStatus::Disabled;
            state.cooldowns.clear();
            // Clear per-model cooldowns: the key is Disabled, so no model
            // is available anyway. Keeps the state tidy.
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

    /// Clear expired per-(key, upstream_model) cooldown entries.
    ///
    /// Called only by the background loop in `main.rs` (every 10s) — NOT
    /// on the per-request path. `is_available` checks `cooldown_until`
    /// inline so availability is unaffected by this loop's cadence; this
    /// method only reclaims memory (drops expired entries) and resets
    /// `consecutive_failures` so the next failure on a (key, model) pair
    /// starts a fresh backoff window rather than continuing an old streak.
    ///
    /// Cooldown is transient — no DB writes happen here. Only `mark_disabled`
    /// and `restore_disabled` persist key status.
    pub async fn check_recoveries(&self) {
        let now = Utc::now();
        {
            let mut map = self.states.write().await;
            for state in map.values_mut() {
                // Retain only entries whose cooldown_until is still in the
                // future. Entries with `cooldown_until = None` are stale
                // (shouldn't normally happen) and are dropped.
                state.cooldowns.retain(|_, entry| {
                    entry
                        .cooldown_until
                        .map(|until| until > now)
                        .unwrap_or(false)
                });
            }
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
                state.cooldowns.clear();
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
            // Re-enabling a key → clear any stale per-model cooldowns so
            // it gets a fresh start.
            state.cooldowns.clear();
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
    async fn test_mark_cooldown_sets_per_model_window() {
        let h = HealthManager::new();
        assert!(h.is_available(1, "model-a").await);

        h.mark_cooldown(1, "model-a").await;
        let s = h.get_state(1).await.unwrap();
        // status 保持 Active — cooldown 只影响 (key, "model-a")，不影响 key 本身
        assert_eq!(s.status, KeyStatus::Active);
        let entry = s.cooldowns.get("model-a").expect("cooldown entry for model-a");
        assert!(entry.cooldown_until.is_some());
        assert_eq!(entry.consecutive_failures, 1);
        // (key, "model-a") 不可用
        assert!(!h.is_available(1, "model-a").await);
    }

    #[tokio::test]
    async fn test_cooldown_isolated_per_upstream_model() {
        // 核心回归测试：同一 key 绑定到多个 upstream_model 时，
        // 一个模型冷却不影响其他模型。这是方案 B 修复的关键场景：
        //   key 1 绑定 "deepseek-chat" 和 "gemini-flash"
        //   deepseek-chat 超时 → (key1, "deepseek-chat") 冷却
        //   gemini-flash 仍然可用 → 不应被跳过
        let h = HealthManager::new();

        // 初始：两个模型都可用
        assert!(h.is_available(1, "deepseek-chat").await);
        assert!(h.is_available(1, "gemini-flash").await);

        // deepseek-chat 失败 → 只冷却 (key1, "deepseek-chat")
        h.mark_cooldown(1, "deepseek-chat").await;

        // deepseek-chat 不可用，但 gemini-flash 仍然可用
        assert!(!h.is_available(1, "deepseek-chat").await, "deepseek-chat should be in cooldown");
        assert!(h.is_available(1, "gemini-flash").await, "gemini-flash must remain available");

        // key.status 仍是 Active（没被 cooldown 影响）
        let s = h.get_state(1).await.unwrap();
        assert_eq!(s.status, KeyStatus::Active);
        // 只有 deepseek-chat 在 cooldowns map 中
        assert_eq!(s.cooldowns.len(), 1);
        assert!(s.cooldowns.contains_key("deepseek-chat"));
        assert!(!s.cooldowns.contains_key("gemini-flash"));
    }

    #[tokio::test]
    async fn test_cooldown_expired_zero_latency_recovery() {
        // 关键回归测试：cooldown 到期后 is_available 必须立即返回 true，
        // 不依赖 check_recoveries。后台 check_recoveries 只负责清理
        // 过期条目（内存回收），不在可用性关键路径上。
        let h = HealthManager::new();

        // 设置一个已过期的 per-model cooldown（模拟冷却到期但
        // check_recoveries 还没跑的场景）
        {
            let mut map = h.states.write().await;
            let state = map.entry(1).or_insert_with(|| KeyRuntimeState::new(1));
            state.cooldowns.insert(
                "model-a".to_string(),
                CooldownEntry {
                    cooldown_until: Some(Utc::now() - Duration::seconds(1)), // 1秒前到期
                    consecutive_failures: 3,
                },
            );
        }

        // 冷却到期 → 立即可用
        assert!(h.is_available(1, "model-a").await, "expired cooldown should be available");
        assert!(h.try_is_available(1, "model-a"), "expired cooldown should be available (sync)");

        // 条目仍在 map 中（check_recoveries 才会清理），但不影响可用性
        let s = h.get_state(1).await.unwrap();
        assert!(s.cooldowns.contains_key("model-a"), "entry not yet cleaned by check_recoveries");
    }

    #[tokio::test]
    async fn test_cooldown_not_yet_expired_unavailable() {
        // 对比测试：cooldown 未到期 → 不可用
        let h = HealthManager::new();
        h.mark_cooldown(1, "model-a").await; // 默认 cooldown 至少 2 秒

        // 冷却未到期 → 不可用
        assert!(!h.is_available(1, "model-a").await, "active cooldown should be unavailable");
        assert!(!h.try_is_available(1, "model-a"), "active cooldown should be unavailable (sync)");
    }

    #[tokio::test]
    async fn test_exponential_backoff_grows_then_caps() {
        let h = HealthManager::new();
        // Track the cooldown window after each failure.
        let mut windows: Vec<i64> = Vec::new();
        for _ in 0..12 {
            h.mark_cooldown(1, "model-a").await;
            let s = h.get_state(1).await.unwrap();
            let entry = s.cooldowns.get("model-a").expect("entry exists");
            let now = Utc::now();
            let secs = entry
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
                    status: KeyStatus::Active,
                    cooldowns: {
                        let mut m = HashMap::new();
                        m.insert(
                            "model-a".to_string(),
                            CooldownEntry {
                                cooldown_until: Some(Utc::now() - Duration::seconds(60)),
                                consecutive_failures: 3,
                            },
                        );
                        m
                    },
                    used_quota_this_period: 0,
                },
            );
        }
        // 过期 cooldown 在 check_recoveries 之前就已可用（零延迟恢复）。
        assert!(h.is_available(1, "model-a").await);

        // check_recoveries 的职责：清理过期的 cooldown 条目（内存回收 +
        // 重置 consecutive_failures，使下次失败重新开始退避）。
        h.check_recoveries().await;
        let s = h.get_state(1).await.unwrap();
        assert!(s.cooldowns.is_empty(), "expired entry should be cleaned up");
        assert!(h.is_available(1, "model-a").await);
    }

    #[tokio::test]
    async fn test_check_recoveries_keeps_active_cooldowns() {
        // 对比测试：未过期的 cooldown 不应被 check_recoveries 清理
        let h = HealthManager::new();
        h.mark_cooldown(1, "model-a").await; // 至少 2 秒未到期

        h.check_recoveries().await;
        let s = h.get_state(1).await.unwrap();
        assert!(s.cooldowns.contains_key("model-a"), "active cooldown should not be cleared");
        assert!(!h.is_available(1, "model-a").await);
    }

    #[tokio::test]
    async fn test_is_available_active_unseen_disabled() {
        let h = HealthManager::new();

        // unseen key → available
        assert!(h.is_available(999, "any-model").await);

        // disabled key → unavailable (for any model)
        h.mark_disabled(2).await;
        assert!(!h.is_available(2, "model-a").await);
        assert!(!h.is_available(2, "model-b").await);
        let s = h.get_state(2).await.unwrap();
        assert_eq!(s.status, KeyStatus::Disabled);

        // restore_disabled brings it back
        h.restore_disabled(vec![2]).await;
        assert!(h.is_available(2, "model-a").await);
        let s = h.get_state(2).await.unwrap();
        assert_eq!(s.status, KeyStatus::Active);
        assert!(s.cooldowns.is_empty());
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
        assert!(!h.is_available(5, "model-a").await);

        // Simulate admin re-enabling the key via DB → memory sync.
        h.sync_key_from_db(5, KeyStatus::Active).await;
        assert!(h.is_available(5, "model-a").await);
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
        assert!(h.is_available(1, "any-model").await);

        // Load disabled keys from DB.
        h.load_disabled_from_db().await;

        // Now key 1 should be unavailable.
        assert!(!h.is_available(1, "any-model").await);
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
