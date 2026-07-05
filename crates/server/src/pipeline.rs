//! Storage adapter — bridges the storage layer (rusqlite repos) and the
//! core layer (CacheLoader, BillingRepo, UsageWriter traits).
//!
//! A single `StorageAdapter` wraps an `Arc<Mutex<Connection>>` and
//! implements all three traits, so the executor/cache/tracker can talk
//! to storage without knowing about rusqlite.

use std::collections::HashMap;
#[cfg(test)]
use std::sync::Arc;

use async_trait::async_trait;
#[cfg(test)]
use tokio::sync::Mutex;

use chennix_common::{
    KeyConfig, ProxyResult, Usage,
};
use chennix_storage::channels::{ChannelRepo, DiscoveredModelRepo};
use chennix_storage::keys::KeyRepo;
use chennix_storage::models::ModelRepo;
use chennix_storage::tokens::TokenRepo;
use chennix_storage::usage::UsageRepo;
use chennix_storage::users::UserRepo;

use chennix_core::billing::BillingRepo as BillingRepoTrait;
use chennix_core::cache::{
    Binding, CacheData, CacheLoader, QuotaState, QuotaStatus, QuotaUnit, QuotaWindow,
    RoutingStrategy,
};
use chennix_core::tracker::UsageWriter;

use crate::state::SharedDb;

pub struct StorageAdapter {
    db: SharedDb,
}

impl StorageAdapter {
    pub fn new(db: SharedDb) -> Self {
        Self { db }
    }

    #[allow(dead_code)]
    pub fn db(&self) -> &SharedDb {
        &self.db
    }
}

#[async_trait]
impl CacheLoader for StorageAdapter {
    async fn load_all(&self) -> ProxyResult<CacheData> {
        let db = self.db.lock().await;

        let ch_repo = ChannelRepo::new(&db);
        let channels = ch_repo.list_channels()?;

        let key_repo = KeyRepo::new(&db);
        let mut keys: HashMap<i64, Vec<KeyConfig>> = HashMap::new();
        for ch in &channels {
            let ch_keys = key_repo.get_keys_for_channel(ch.id)?;
            keys.insert(ch.id, ch_keys);
        }

        let model_repo = ModelRepo::new(&db);
        let all_models = model_repo.list_all_models()?;
        let mut bindings: HashMap<i64, Vec<Binding>> = HashMap::new();
        let mut channel_model_pricing: HashMap<(i64, i64, String), chennix_common::ChannelModelPricing> =
            HashMap::new();
        let mut routing_strategy: HashMap<i64, RoutingStrategy> = HashMap::new();
        for (model_id, _canonical_name, strategy_str) in &all_models {
            // 路由策略（列有 NOT NULL DEFAULT 'priority'；未知值回退到 Priority）。
            routing_strategy.insert(*model_id, RoutingStrategy::from_db_str(strategy_str));

            let model_bindings = model_repo.get_bindings_for_model(*model_id)?;
            // get_bindings_for_model 已按 priority 升序返回；这里再按 priority
            // 稳定排序一次以与历史行为保持一致（同 priority 时保留 channel_id 顺序）。
            let mut binding_list: Vec<Binding> = model_bindings
                .iter()
                .map(|b| Binding {
                    channel_id: b.channel_id,
                    upstream_model_name: b.upstream_model_name.clone(),
                    priority: b.priority,
                    weight: b.weight,
                })
                .collect();
            binding_list.sort_by_key(|b| b.priority);
            bindings.insert(*model_id, binding_list);
            // 加载每个绑定的定价配置（key 为三元组 model_id, channel_id, upstream）。
            for b in &model_bindings {
                if let Some(pricing) =
                    model_repo.get_binding_pricing(*model_id, b.channel_id, &b.upstream_model_name)?
                {
                    channel_model_pricing.insert(
                        (*model_id, b.channel_id, b.upstream_model_name.clone()),
                        pricing,
                    );
                }
            }
        }

        // 小模型额度快照：(channel_id, raw_model_name) → QuotaState。
        // upstream_model_name 恒等于 raw_model_name，故 key 与绑定侧一致。
        let discovered_repo = DiscoveredModelRepo::new(&db);
        let mut small_model_quota: HashMap<(i64, String), QuotaState> = HashMap::new();
        for dm in discovered_repo.list_discovered_models()? {
            small_model_quota.insert(
                (dm.channel_id, dm.raw_model_name.clone()),
                QuotaState {
                    limit: dm.quota_limit,
                    unit: dm.quota_unit.as_deref().and_then(QuotaUnit::from_db_str),
                    window: dm.quota_window.as_deref().and_then(QuotaWindow::from_db_str),
                    used: dm.used_quota,
                    last_reset_at: dm.last_reset_at.clone(),
                    status: QuotaStatus::from_db_str(&dm.quota_status),
                },
            );
        }

        Ok(CacheData {
            channels,
            keys,
            bindings,
            channel_model_pricing,
            routing_strategy,
            small_model_quota,
        })
    }

    async fn load_alias_mapping(&self) -> ProxyResult<HashMap<String, (i64, String)>> {
        let db = self.db.lock().await;
        let model_repo = ModelRepo::new(&db);
        let all_models = model_repo.list_all_models()?;

        let mut mapping: HashMap<String, (i64, String)> = HashMap::new();
        for (model_id, canonical_name, _strategy) in &all_models {
            // canonical_name → itself (大小写不敏感由 Normalizer 处理)
            mapping.insert(canonical_name.clone(), (*model_id, canonical_name.clone()));
        }
        Ok(mapping)
    }
}

#[async_trait]
impl BillingRepoTrait for StorageAdapter {
    async fn get_user_quota(&self, user_id: i64) -> ProxyResult<Option<i64>> {
        let db = self.db.lock().await;
        let repo = UserRepo::new(&db);
        let user = repo.get_user_by_id(user_id)?;
        Ok(user.map(|u| u.quota - u.used_quota))
    }

    async fn get_token_remain_quota(&self, token_id: i64) -> ProxyResult<Option<i64>> {
        let db = self.db.lock().await;
        let repo = TokenRepo::new(&db);
        repo.get_remain_quota(token_id)
    }

    async fn update_token_status(&self, token_id: i64, status: i32) -> ProxyResult<()> {
        let db = self.db.lock().await;
        let repo = TokenRepo::new(&db);
        repo.update_status(token_id, status)
    }

    async fn get_token_unlimited(&self, token_id: i64) -> ProxyResult<Option<bool>> {
        let db = self.db.lock().await;
        let repo = TokenRepo::new(&db);
        let token = repo.get_token_by_id(token_id)?;
        Ok(token.map(|t| t.unlimited_quota))
    }

    /// 单事务预扣：user 层 + token 层（非 unlimited）在同一事务内
    /// 检查余额并扣减。任一层不足或失败则 ROLLBACK。
    ///
    /// SQL 用 `WHERE (quota - used_quota) >= ?` 做条件扣减，
    /// 通过 `changes()` 判断是否扣减成功，避免 TOCTOU。
    async fn pre_charge_atomic(
        &self,
        user_id: i64,
        token_id: i64,
        amount: i64,
        token_unlimited: bool,
    ) -> ProxyResult<()> {
        use rusqlite::params;
        let db = self.db.lock().await;

        // 开启事务
        let tx = db
            .unchecked_transaction()
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

        // 1. user 层：条件扣减（quota - used_quota >= amount）
        let now = chennix_storage::now_iso8601();
        let user_changes = tx
            .execute(
                "UPDATE users SET used_quota = used_quota + ?1, updated_at = ?2
                 WHERE id = ?3 AND (quota - used_quota) >= ?1",
                params![amount, now, user_id],
            )
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        if user_changes == 0 {
            // user 不存在或余额不足 → 回滚
            let _ = tx.rollback();
            return Err(chennix_common::ProxyError::Config(format!(
                "insufficient user quota: user_id={} needed={}",
                user_id, amount
            )));
        }

        // 2. token 层（非 unlimited）：条件扣减
        if !token_unlimited {
            let token_changes = tx
                .execute(
                    "UPDATE tokens
                     SET remain_quota = remain_quota - ?1,
                         used_quota = used_quota + ?1,
                         updated_at = ?2
                     WHERE id = ?3 AND remain_quota >= ?1",
                    params![amount, now, token_id],
                )
                .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
            if token_changes == 0 {
                // token 不存在或余额不足 → 回滚（user 层扣减也被撤销）
                let _ = tx.rollback();
                return Err(chennix_common::ProxyError::Config(format!(
                    "insufficient token quota: token_id={} needed={}",
                    token_id, amount
                )));
            }
        }

        // 3. 提交事务
        tx.commit()
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 单事务结算：调整 `delta`（可为负）到 user 层 + token 层（非 unlimited）。
    /// 不检查余额，允许透支（与 new-api 一致）。
    async fn settle_atomic(
        &self,
        user_id: i64,
        token_id: i64,
        delta: i64,
        token_unlimited: bool,
    ) -> ProxyResult<()> {
        use rusqlite::params;
        let db = self.db.lock().await;

        let tx = db
            .unchecked_transaction()
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

        // user 层：无条件调整
        let now = chennix_storage::now_iso8601();
        tx.execute(
            "UPDATE users SET used_quota = used_quota + ?1, updated_at = ?2
             WHERE id = ?3",
            params![delta, now, user_id],
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

        // token 层（非 unlimited）：无条件调整
        if !token_unlimited {
            tx.execute(
                "UPDATE tokens
                 SET remain_quota = remain_quota - ?1,
                     used_quota = used_quota + ?1,
                     updated_at = ?2
                 WHERE id = ?3",
                params![delta, now, token_id],
            )
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        }

        tx.commit()
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 单事务退款：退还预扣的 `amount` 到 user 层 + token 层（非 unlimited）。
    async fn refund_atomic(
        &self,
        user_id: i64,
        token_id: i64,
        amount: i64,
        token_unlimited: bool,
    ) -> ProxyResult<()> {
        use rusqlite::params;
        let db = self.db.lock().await;

        let tx = db
            .unchecked_transaction()
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

        // user 层：退还
        let now = chennix_storage::now_iso8601();
        tx.execute(
            "UPDATE users SET used_quota = used_quota - ?1, updated_at = ?2
             WHERE id = ?3",
            params![amount, now, user_id],
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

        // token 层（非 unlimited）：退还
        if !token_unlimited {
            tx.execute(
                "UPDATE tokens
                 SET remain_quota = remain_quota + ?1,
                     used_quota = used_quota - ?1,
                     updated_at = ?2
                 WHERE id = ?3",
                params![amount, now, token_id],
            )
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        }

        tx.commit()
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        Ok(())
    }
}

#[async_trait]
impl UsageWriter for StorageAdapter {
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
        let db = self.db.lock().await;
        let repo = UsageRepo::new(&db);
        repo.log_usage(
            channel_id,
            key_id,
            model_id,
            usage,
            request_type,
            status,
            error,
            user_id,
            token_id,
            quota_cost,
        )?;
        Ok(())
    }

    async fn add_key_usage(&self, key_id: i64, tokens: u64) -> ProxyResult<()> {
        let db = self.db.lock().await;
        let repo = KeyRepo::new(&db);
        repo.add_key_usage(key_id, tokens)
    }

    async fn add_small_model_usage(
        &self,
        channel_id: i64,
        upstream_model_name: &str,
        delta: i64,
        quota_status: &str,
    ) -> ProxyResult<()> {
        let db = self.db.lock().await;
        let repo = DiscoveredModelRepo::new(&db);
        repo.add_discovered_model_usage(channel_id, upstream_model_name, delta, quota_status)
    }

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
        upstream_model: Option<&str>,
        response_status: i64,
        duration_ms: i64,
        stream: bool,
        error_message: Option<&str>,
        user_id: Option<i64>,
        token_id: Option<i64>,
        quota_cost: i64,
    ) -> ProxyResult<()> {
        let db = self.db.lock().await;
        let repo = UsageRepo::new(&db);
        repo.log_request(
            request_id,
            client_ip,
            method,
            path,
            client_model,
            normalized_model,
            channel_name,
            key_label,
            attempted_keys,
            upstream_status,
            upstream_model,
            response_status,
            duration_ms,
            stream,
            error_message,
            user_id,
            token_id,
            quota_cost,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chennix_storage::schema::init_db;
    use rusqlite::Connection;

    async fn setup() -> (StorageAdapter, Arc<Mutex<Connection>>) {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // seed a channel + key + model + binding + user + token
        conn.execute(
            "INSERT INTO channels (id, name, provider, base_url, \"group\")
             VALUES (1, 'ch1', 'openai-compatible', 'http://up', 'default')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO channel_keys (id, channel_id, api_key, cost_tier, key_priority, status)
             VALUES (10, 1, 'sk-up', 'paid', 100, 'active')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO models (id, canonical_name) VALUES (1, 'gpt-4')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO model_channels (model_id, channel_id, upstream_model_name, priority)
             VALUES (1, 1, 'gpt-4-upstream', 100)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO users (id, username, password_hash, role, status, quota, used_quota, \"group\")
             VALUES (1, 'alice', 'h', 1, 1, 1000, 0, 'default')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tokens (id, user_id, key, remain_quota, used_quota, unlimited_quota,
                                 expired_time, model_limits_enabled, status)
             VALUES (1, 1, 'sk-alice', 500, 0, 0, -1, 0, 1)",
            [],
        )
        .unwrap();

        let db = Arc::new(Mutex::new(conn));
        let adapter = StorageAdapter::new(db.clone());
        (adapter, db)
    }

    #[tokio::test]
    async fn test_cache_loader_load_all() {
        let (adapter, _) = setup().await;
        let data = adapter.load_all().await.unwrap();
        assert_eq!(data.channels.len(), 1);
        assert_eq!(data.channels[0].name, "ch1");
        assert_eq!(data.keys.len(), 1);
        assert_eq!(data.keys.get(&1).unwrap().len(), 1);
        assert_eq!(data.bindings.len(), 1);
        let b = data.bindings.get(&1).unwrap();
        assert_eq!(b[0].channel_id, 1); // channel_id
        assert_eq!(b[0].upstream_model_name, "gpt-4-upstream");
        assert_eq!(b[0].priority, 100); // priority
        assert_eq!(b[0].weight, 1); // weight 默认 1
    }

    #[tokio::test]
    async fn test_cache_loader_load_alias_mapping() {
        let (adapter, _) = setup().await;
        let mapping = adapter.load_alias_mapping().await.unwrap();
        // canonical name → itself
        assert_eq!(mapping.get("gpt-4"), Some(&(1, "gpt-4".to_string())));
    }

    #[tokio::test]
    async fn test_billing_repo_user_quota() {
        let (adapter, _) = setup().await;
        // alice has quota=1000, used=0 → remaining 1000
        let q = adapter.get_user_quota(1).await.unwrap();
        assert_eq!(q, Some(1000));
    }

    #[tokio::test]
    async fn test_billing_repo_pre_charge_atomic() {
        let (adapter, _) = setup().await;
        // 预扣 200：user 层 + token 层都应扣减
        adapter.pre_charge_atomic(1, 1, 200, false).await.unwrap();
        let q = adapter.get_user_quota(1).await.unwrap();
        assert_eq!(q, Some(800), "user remaining should drop by 200");
        let tq = adapter.get_token_remain_quota(1).await.unwrap();
        assert_eq!(tq, Some(300), "token remain should drop by 200");
    }

    #[tokio::test]
    async fn test_billing_repo_pre_charge_atomic_insufficient() {
        let (adapter, _) = setup().await;
        // 预扣 2000（超过 user 余额 1000）→ 应失败且无副作用
        let err = adapter.pre_charge_atomic(1, 1, 2000, false).await.unwrap_err();
        assert!(matches!(err, chennix_common::ProxyError::Config(_)));
        let q = adapter.get_user_quota(1).await.unwrap();
        assert_eq!(q, Some(1000), "user layer must not be touched on failure");
        let tq = adapter.get_token_remain_quota(1).await.unwrap();
        assert_eq!(tq, Some(500), "token layer must not be touched on failure");
    }

    #[tokio::test]
    async fn test_billing_repo_token_remain_quota() {
        let (adapter, _) = setup().await;
        let q = adapter.get_token_remain_quota(1).await.unwrap();
        assert_eq!(q, Some(500));
    }

    #[tokio::test]
    async fn test_billing_repo_token_unlimited() {
        let (adapter, _) = setup().await;
        let u = adapter.get_token_unlimited(1).await.unwrap();
        assert_eq!(u, Some(false));
    }

    #[tokio::test]
    async fn test_billing_repo_missing_user_returns_none() {
        let (adapter, _) = setup().await;
        assert_eq!(adapter.get_user_quota(999).await.unwrap(), None);
    }

    #[tokio::test]
    async fn test_usage_writer_log_usage() {
        let (adapter, db) = setup().await;
        let usage = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
        };
        adapter
            .log_usage(1, 1, 1, 10, 1, &usage, 30, "chat", "success", None)
            .await
            .unwrap();

        // verify the row was written
        let conn = db.lock().await;
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM usage_logs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn test_usage_writer_add_key_usage() {
        let (adapter, db) = setup().await;
        adapter.add_key_usage(10, 42).await.unwrap();

        let conn = db.lock().await;
        let used: i64 = conn
            .query_row(
                "SELECT used_quota FROM channel_keys WHERE id = 10",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(used, 42);
    }
}
