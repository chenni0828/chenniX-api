//! Process-local configuration cache.
//!
//! Holds a snapshot of (channels, keys, model bindings) so that the router
//! does not have to round-trip through the database on every request. The
//! cache is lazy — `get` loads on first access — and explicitely
//! invalidated by `invalidate` (called by admin endpoints that mutate
//! config). The alias → model mapping is delegated to the `Normalizer`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chennix_common::{ChannelConfig, ChannelModelPricing, KeyConfig, ProxyResult};
use tokio::sync::RwLock;

use crate::normalizer::Normalizer;

/// 大模型路由策略。
///
/// 数据库存储为字符串 `'priority'` / `'load_balance'`（见
/// `models.routing_strategy`），由 `from_db_str` 解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RoutingStrategy {
    /// 按优先级顺序依次尝试（数字越小越优先）。
    #[default]
    Priority,
    /// 按 weight 加权随机选择，失败时从剩余候选中再按权重随机。
    LoadBalance,
}

impl RoutingStrategy {
    /// 从数据库存储的字符串解析；未知值回退到 `Priority`。
    pub fn from_db_str(s: &str) -> Self {
        match s.trim() {
            "load_balance" => Self::LoadBalance,
            _ => Self::Priority,
        }
    }
}

/// 小模型额度计量单位（`discovered_models.quota_unit`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaUnit {
    /// 按 token 计量。
    Token,
    /// 按调用次数计量。
    Call,
}

impl QuotaUnit {
    /// 从数据库存储的字符串解析；未知/空值返回 `None`。
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s.trim() {
            "token" => Some(Self::Token),
            "call" => Some(Self::Call),
            _ => None,
        }
    }
}

/// 小模型额度重置周期（`discovered_models.quota_window`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaWindow {
    Day,
    Month,
    Total,
}

impl QuotaWindow {
    /// 从数据库存储的字符串解析；未知/空值返回 `None`。
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s.trim() {
            "day" => Some(Self::Day),
            "month" => Some(Self::Month),
            "total" => Some(Self::Total),
            _ => None,
        }
    }
}

/// 小模型额度状态（`discovered_models.quota_status`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaStatus {
    Available,
    Exhausted,
}

impl QuotaStatus {
    /// 从数据库存储的字符串解析；未知值回退到 `Available`。
    pub fn from_db_str(s: &str) -> Self {
        match s.trim() {
            "exhausted" => Self::Exhausted,
            _ => Self::Available,
        }
    }

    /// 序列化为数据库存储的字符串。
    pub fn as_db_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Exhausted => "exhausted",
        }
    }
}

/// 小模型额度快照。
///
/// 归属以 `(channel_id, upstream_model_name)` 为标识（即小模型二元组
/// 本身，等价于 `discovered_models.(channel_id, raw_model_name)`）。
/// `limit = None` 表示无限制。
#[derive(Debug, Clone)]
pub struct QuotaState {
    /// `None` 表示无限制。
    pub limit: Option<i64>,
    pub unit: Option<QuotaUnit>,
    pub window: Option<QuotaWindow>,
    pub used: i64,
    pub last_reset_at: Option<String>,
    pub status: QuotaStatus,
}

/// 缓存内的一条模型绑定（大模型 → 渠道小模型）。
///
/// 等价于 `model_channels` 一行：`(model_id, channel_id,
/// upstream_model_name)` 三元组 + priority + weight。
#[derive(Debug, Clone)]
pub struct Binding {
    pub channel_id: i64,
    pub upstream_model_name: String,
    /// 数字越小越优先（priority 策略排序依据）。
    pub priority: i32,
    /// 负载均衡权重（>=1，仅 load_balance 策略生效）。
    pub weight: i32,
}

/// Cached configuration snapshot.
#[derive(Debug, Clone, Default)]
pub struct CacheData {
    /// All channels.
    pub channels: Vec<ChannelConfig>,
    /// `channel_id → keys` (active + inactive; the router filters).
    pub keys: HashMap<i64, Vec<KeyConfig>>,
    /// `model_id → Vec<Binding>`。
    /// 每个绑定的 priority 用于路由排序（数字越小越优先），由模型管理页配置。
    pub bindings: HashMap<i64, Vec<Binding>>,
    /// `(model_id, channel_id, upstream_model_name) → ChannelModelPricing`。
    /// 同一模型在同一渠道的不同 upstream 绑定可有不同定价。
    pub channel_model_pricing: HashMap<(i64, i64, String), ChannelModelPricing>,
    /// `model_id → routing_strategy`。未出现的 model_id 回退到 `Priority`。
    pub routing_strategy: HashMap<i64, RoutingStrategy>,
    /// `(channel_id, upstream_model_name) → QuotaState`，小模型额度快照。
    pub small_model_quota: HashMap<(i64, String), QuotaState>,
}

impl CacheData {
    /// Resolve a model_id to its bindings.
    pub fn bindings_for(&self, model_id: i64) -> Option<&Vec<Binding>> {
        self.bindings.get(&model_id)
    }

    /// Look up a channel by id.
    pub fn channel(&self, channel_id: i64) -> Option<&ChannelConfig> {
        self.channels.iter().find(|c| c.id == channel_id)
    }

    /// Look up the keys for a channel.
    pub fn keys_for(&self, channel_id: i64) -> Option<&Vec<KeyConfig>> {
        self.keys.get(&channel_id)
    }

    /// Look up the per-binding pricing for a
    /// `(model_id, channel_id, upstream_model_name)`.
    pub fn pricing_for(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream: &str,
    ) -> Option<&ChannelModelPricing> {
        self.channel_model_pricing
            .get(&(model_id, channel_id, upstream.to_string()))
    }

    /// 该大模型的路由策略；未配置时回退到默认 `Priority`。
    pub fn routing_strategy_for(&self, model_id: i64) -> RoutingStrategy {
        self.routing_strategy.get(&model_id).copied().unwrap_or_default()
    }

    /// 某个小模型 `(channel_id, upstream)` 的额度快照。
    pub fn small_model_quota_for(&self, channel_id: i64, upstream: &str) -> Option<&QuotaState> {
        self.small_model_quota
            .get(&(channel_id, upstream.to_string()))
    }
}

/// Backend that loads config from storage. A thin adapter around
/// `ChannelRepo` + `KeyRepo` + `ModelRepo` will implement this in Task 23.
#[async_trait]
pub trait CacheLoader: Send + Sync {
    /// Load the full cache snapshot (channels + their keys + all bindings).
    async fn load_all(&self) -> ProxyResult<CacheData>;
    /// Load the alias → (model_id, canonical_name) mapping for the normalizer.
    async fn load_alias_mapping(&self) -> ProxyResult<HashMap<String, (i64, String)>>;
}

pub struct ConfigCache {
    data: Arc<RwLock<Option<CacheData>>>,
    normalizer: Arc<Normalizer>,
}

impl ConfigCache {
    pub fn new(normalizer: Arc<Normalizer>) -> Self {
        Self {
            data: Arc::new(RwLock::new(None)),
            normalizer,
        }
    }

    /// Drop the cached snapshot. The next `get` will trigger a fresh load.
    pub async fn invalidate(&self) {
        let mut w = self.data.write().await;
        *w = None;
    }

    /// Return a clone of the cached snapshot, loading it on first access.
    /// Also (re)loads the normalizer mapping if the cache was empty.
    pub async fn get(&self, loader: &dyn CacheLoader) -> ProxyResult<CacheData> {
        // Fast path: already cached.
        {
            let r = self.data.read().await;
            if let Some(d) = r.as_ref() {
                return Ok(d.clone());
            }
        }
        // Slow path: load.
        let snapshot = loader.load_all().await?;
        let alias_map = loader.load_alias_mapping().await?;
        self.normalizer.reload(alias_map).await;
        let mut w = self.data.write().await;
        // Another concurrent loader may have written first; last-writer-wins
        // is fine here since both loaded from the same source.
        *w = Some(snapshot.clone());
        Ok(snapshot)
    }

    /// Return the routed candidates for a specific model + user_group.
    ///
    /// Steps:
    /// 1. Load (or reuse) the cached snapshot.
    /// 2. Look up `model_id` in `bindings`.
    /// 3. For each `Binding (channel_id, upstream_model_name, priority, weight)`:
    ///    - find the channel by id (skip if missing)
    ///    - find the keys for that channel (skip if empty)
    ///    - emit `(channel, keys, upstream_model_name, priority)`
    ///
    /// Group filtering is delegated to `Router::route` — `get_for_model`
    /// returns *all* bound channels and lets the router apply the group
    /// check, since the group filter depends on the caller's user_group
    /// which varies per request.
    ///
    /// NOTE: `weight` 由 Router（Task 3）消费；`routing_strategy` 通过
    /// [`ConfigCache::routing_strategy_for`] 单独获取。
    pub async fn get_for_model(
        &self,
        model_id: i64,
        _user_group: &str,
        loader: &dyn CacheLoader,
    ) -> ProxyResult<Vec<(ChannelConfig, Vec<KeyConfig>, String, i32, i32)>> {
        let snapshot = self.get(loader).await;

        let snapshot = snapshot?;

        let Some(bindings) = snapshot.bindings_for(model_id) else {
            return Ok(Vec::new());
        };

        let mut out: Vec<(ChannelConfig, Vec<KeyConfig>, String, i32, i32)> = Vec::new();
        for b in bindings {
            let Some(channel) = snapshot.channel(b.channel_id) else {
                continue;
            };
            let keys = snapshot
                .keys_for(b.channel_id)
                .cloned()
                .unwrap_or_default();
            if keys.is_empty() {
                continue;
            }
            out.push((
                channel.clone(),
                keys,
                b.upstream_model_name.clone(),
                b.priority,
                b.weight,
            ));
        }
        Ok(out)
    }

    /// 该大模型的路由策略；未配置时回退到默认 `Priority`。
    ///
    /// 加载缓存（首次访问时）后从快照读取。已缓存时仅做一次克隆，无 DB 往返。
    pub async fn routing_strategy_for(
        &self,
        model_id: i64,
        loader: &dyn CacheLoader,
    ) -> ProxyResult<RoutingStrategy> {
        let snapshot = self.get(loader).await?;
        Ok(snapshot.routing_strategy_for(model_id))
    }

    /// Look up the per-binding pricing for a
    /// `(model_id, channel_id, upstream_model_name)`. Loads the cache on
    /// first access, then reads from the in-memory map.
    pub async fn get_channel_model_pricing(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream: &str,
        loader: &dyn CacheLoader,
    ) -> ProxyResult<Option<ChannelModelPricing>> {
        let snapshot = self.get(loader).await?;
        Ok(snapshot.pricing_for(model_id, channel_id, upstream).cloned())
    }

    /// 读取某个小模型 `(channel_id, upstream)` 的额度快照（克隆）。
    ///
    /// 不需要 `CacheLoader`：仅从已加载的缓存快照读取。若缓存尚未加载
    /// （`None`）或对应条目不存在，返回 `None` —— 调用方据此跳过累加。
    pub async fn get_small_model_quota(
        &self,
        channel_id: i64,
        upstream: &str,
    ) -> Option<QuotaState> {
        let r = self.data.read().await;
        r.as_ref()
            .and_then(|d| d.small_model_quota.get(&(channel_id, upstream.to_string())).cloned())
    }

    /// 增量更新某个小模型 `(channel_id, upstream)` 的额度状态。
    ///
    /// 仅更新缓存中已存在条目的 `used` 与 `status` 字段，**不触发全量
    /// 缓存重建**。若缓存尚未加载（`None`）或对应条目不存在，则为
    /// no-op —— 调用方（tracker）通常在请求完成后调用，此时缓存一般
    /// 已加载；若未加载则下一次 `get` 会从数据库读到最新值。
    pub async fn update_small_model_quota(
        &self,
        channel_id: i64,
        upstream: &str,
        used: i64,
        status: QuotaStatus,
    ) {
        let mut w = self.data.write().await;
        if let Some(data) = w.as_mut() {
            if let Some(state) = data
                .small_model_quota
                .get_mut(&(channel_id, upstream.to_string()))
            {
                state.used = used;
                state.status = status;
            }
        }
    }

    /// Accessor for the normalizer (used by the executor / request pipeline
    /// to resolve incoming model names).
    pub fn normalizer(&self) -> &Arc<Normalizer> {
        &self.normalizer
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Mutex;

    /// Mock loader that returns canned data and counts how many times
    /// `load_all` was called.
    struct MockLoader {
        load_count: AtomicU32,
        alias_count: AtomicU32,
        data: Mutex<CacheData>,
        alias: Mutex<HashMap<String, (i64, String)>>,
    }

    impl MockLoader {
        fn new(data: CacheData, alias: HashMap<String, (i64, String)>) -> Self {
            Self {
                load_count: AtomicU32::new(0),
                alias_count: AtomicU32::new(0),
                data: Mutex::new(data),
                alias: Mutex::new(alias),
            }
        }
        fn load_count(&self) -> u32 {
            self.load_count.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl CacheLoader for MockLoader {
        async fn load_all(&self) -> ProxyResult<CacheData> {
            self.load_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.data.lock().unwrap().clone())
        }
        async fn load_alias_mapping(&self) -> ProxyResult<HashMap<String, (i64, String)>> {
            self.alias_count.fetch_add(1, Ordering::SeqCst);
            Ok(self.alias.lock().unwrap().clone())
        }
    }

    fn sample_data() -> CacheData {
        use chennix_common::{ChannelProvider, CostTier, KeyStatus};

        let ch1 = ChannelConfig {
            id: 1, name: "ch1".into(), provider: ChannelProvider::OpenaiCompatible,
            base_url: "http://ch1".into(), group: "default".into(),
        };
        let ch2 = ChannelConfig {
            id: 2, name: "ch2".into(), provider: ChannelProvider::Anthropic,
            base_url: "http://ch2".into(), group: "default,vip".into(),
        };

        let k1 = KeyConfig {
            id: 10, channel_id: 1, api_key: "sk-1".into(), label: None,
            cost_tier: CostTier::Paid, key_priority: 100, price_per_1k_tokens: Some(0.01),
            free_quota: None, used_quota: 0, quota_reset_period: None, status: KeyStatus::Active,
        };
        let k2 = KeyConfig {
            id: 11, channel_id: 2, api_key: "sk-2".into(), label: None,
            cost_tier: CostTier::Free, key_priority: 100, price_per_1k_tokens: Some(0.0),
            free_quota: Some(1000), used_quota: 0, quota_reset_period: None, status: KeyStatus::Active,
        };

        let mut keys = HashMap::new();
        keys.insert(1, vec![k1]);
        keys.insert(2, vec![k2]);

        let mut bindings = HashMap::new();
        bindings.insert(
            7,
            vec![
                Binding {
                    channel_id: 1,
                    upstream_model_name: "gpt-4-upstream".into(),
                    priority: 10,
                    weight: 1,
                },
                Binding {
                    channel_id: 2,
                    upstream_model_name: "claude-upstream".into(),
                    priority: 20,
                    weight: 1,
                },
            ],
        );

        let mut channel_model_pricing = HashMap::new();
        channel_model_pricing.insert(
            (7, 1, "gpt-4-upstream".to_string()),
            chennix_common::ChannelModelPricing {
                billing_type: chennix_common::BillingType::Token,
                input_price: 0.03,
                output_price: 0.06,
                call_price: 0.0,
                billing_expr: None,
            },
        );

        let mut routing_strategy = HashMap::new();
        routing_strategy.insert(7, RoutingStrategy::Priority);

        let mut small_model_quota = HashMap::new();
        small_model_quota.insert(
            (1, "gpt-4-upstream".to_string()),
            QuotaState {
                limit: Some(1_000_000),
                unit: Some(QuotaUnit::Token),
                window: Some(QuotaWindow::Month),
                used: 0,
                last_reset_at: None,
                status: QuotaStatus::Available,
            },
        );

        CacheData {
            channels: vec![ch1, ch2],
            keys,
            bindings,
            channel_model_pricing,
            routing_strategy,
            small_model_quota,
        }
    }

    fn sample_alias() -> HashMap<String, (i64, String)> {
        let mut m = HashMap::new();
        m.insert("gpt-4".into(), (7, "gpt-4".into()));
        m.insert("claude".into(), (7, "gpt-4".into()));
        m
    }

    #[tokio::test]
    async fn test_invalidate_forces_reload() {
        let normalizer = Arc::new(Normalizer::new());
        let cache = ConfigCache::new(normalizer);
        let loader = MockLoader::new(sample_data(), sample_alias());

        // First get → loads.
        let d1 = cache.get(&loader).await.unwrap();
        assert_eq!(loader.load_count(), 1);
        assert_eq!(d1.channels.len(), 2);

        // Second get → cached, no reload.
        let _d2 = cache.get(&loader).await.unwrap();
        assert_eq!(loader.load_count(), 1, "second get should not reload");

        // Invalidate → next get reloads.
        cache.invalidate().await;
        let _d3 = cache.get(&loader).await.unwrap();
        assert_eq!(loader.load_count(), 2, "invalidate should force reload");
    }

    #[tokio::test]
    async fn test_get_returns_cached_data() {
        let normalizer = Arc::new(Normalizer::new());
        let cache = ConfigCache::new(normalizer);
        let loader = MockLoader::new(sample_data(), sample_alias());

        let d = cache.get(&loader).await.unwrap();
        assert_eq!(d.channels.len(), 2);
        assert_eq!(d.keys.len(), 2);
        assert_eq!(d.bindings.len(), 1);
        assert_eq!(d.bindings.get(&7).unwrap().len(), 2);

        // Normalizer also got populated from alias map.
        let n = cache.normalizer();
        assert_eq!(
            n.resolve("gpt-4").await.unwrap(),
            Some((7, "gpt-4".to_string()))
        );
        assert_eq!(
            n.resolve("claude").await.unwrap(),
            Some((7, "gpt-4".to_string()))
        );
    }

    #[tokio::test]
    async fn test_get_for_model_returns_bound_channels() {
        let normalizer = Arc::new(Normalizer::new());
        let cache = ConfigCache::new(normalizer);
        let loader = MockLoader::new(sample_data(), sample_alias());

        let out = cache.get_for_model(7, "default", &loader).await.unwrap();
        assert_eq!(out.len(), 2);
        // Both channels bound to model 7
        let channel_ids: Vec<i64> = out.iter().map(|(c, _, _, _, _)| c.id).collect();
        assert!(channel_ids.contains(&1));
        assert!(channel_ids.contains(&2));

        // Each tuple has its keys + upstream name + priority
        for (ch, keys, upstream, _prio, _weight) in &out {
            assert!(!keys.is_empty());
            match ch.id {
                1 => assert_eq!(upstream, "gpt-4-upstream"),
                2 => assert_eq!(upstream, "claude-upstream"),
                _ => panic!("unexpected channel id {}", ch.id),
            }
        }
    }

    #[tokio::test]
    async fn test_get_for_model_unknown_returns_empty() {
        let normalizer = Arc::new(Normalizer::new());
        let cache = ConfigCache::new(normalizer);
        let loader = MockLoader::new(sample_data(), sample_alias());

        let out = cache.get_for_model(999, "default", &loader).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn test_get_for_model_skips_missing_channel() {
        // Build a snapshot where a binding references a channel that doesn't
        // exist in the channels list.
        let mut data = sample_data();
        // Drop channel 2 from the channels list but keep the binding.
        data.channels.retain(|c| c.id != 2);

        let normalizer = Arc::new(Normalizer::new());
        let cache = ConfigCache::new(normalizer);
        let loader = MockLoader::new(data, sample_alias());

        let out = cache.get_for_model(7, "default", &loader).await.unwrap();
        // Only channel 1 should be returned; channel 2 is silently skipped.
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.id, 1);
    }

    #[tokio::test]
    async fn test_routing_strategy_for_falls_back_to_priority() {
        let data = sample_data();
        // model 7 已配置为 Priority
        assert_eq!(data.routing_strategy_for(7), RoutingStrategy::Priority);
        // 未配置的 model 回退到默认 Priority
        assert_eq!(data.routing_strategy_for(999), RoutingStrategy::Priority);

        let mut data_lb = sample_data();
        data_lb.routing_strategy.insert(7, RoutingStrategy::LoadBalance);
        assert_eq!(data_lb.routing_strategy_for(7), RoutingStrategy::LoadBalance);
    }

    #[tokio::test]
    async fn test_pricing_for_uses_triple_key() {
        let data = sample_data();
        // (7, 1, "gpt-4-upstream") 已配置定价
        assert!(data
            .pricing_for(7, 1, "gpt-4-upstream")
            .is_some());
        // 同 channel 不同 upstream 未配置 → None
        assert!(data.pricing_for(7, 1, "other-upstream").is_none());
    }

    #[tokio::test]
    async fn test_update_small_model_quota_is_incremental() {
        let normalizer = Arc::new(Normalizer::new());
        let cache = ConfigCache::new(normalizer);
        let loader = MockLoader::new(sample_data(), sample_alias());

        // 触发首次加载。
        let d = cache.get(&loader).await.unwrap();
        assert_eq!(loader.load_count(), 1);
        let q = d.small_model_quota_for(1, "gpt-4-upstream").unwrap();
        assert_eq!(q.used, 0);
        assert_eq!(q.status, QuotaStatus::Available);

        // 增量更新：不触发 reload。
        cache
            .update_small_model_quota(1, "gpt-4-upstream", 500, QuotaStatus::Exhausted)
            .await;
        assert_eq!(
            loader.load_count(),
            1,
            "incremental update must not reload the cache"
        );

        // 再次 get 拿到的是更新后的快照（未 reload，仍走缓存）。
        let d2 = cache.get(&loader).await.unwrap();
        assert_eq!(loader.load_count(), 1, "second get should not reload");
        let q2 = d2.small_model_quota_for(1, "gpt-4-upstream").unwrap();
        assert_eq!(q2.used, 500);
        assert_eq!(q2.status, QuotaStatus::Exhausted);
    }

    #[tokio::test]
    async fn test_update_small_model_quota_unknown_entry_is_noop() {
        let normalizer = Arc::new(Normalizer::new());
        let cache = ConfigCache::new(normalizer);
        let loader = MockLoader::new(sample_data(), sample_alias());
        let _ = cache.get(&loader).await.unwrap();

        // 不存在的 (channel_id, upstream) 不应 panic，也不应 reload。
        cache
            .update_small_model_quota(999, "missing", 1, QuotaStatus::Exhausted)
            .await;
        assert_eq!(loader.load_count(), 1);
    }
}
