use serde::{Deserialize, Serialize};

/// 1 元 = 1,000,000 微元（内部配额单位）。
///
/// 所有 money-quota 字段（users.quota / tokens.remain_quota /
/// usage_logs.quota_cost / request_logs.quota_cost）均以微元存储，
/// 保证整数运算无精度损失（1 微元 = 0.000001 元，足以精确记录单次
/// token 级别的成本）。参考 new-api 的 QuotaPerUnit 设计。
pub const QUOTA_PER_YUAN: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChannelProvider {
    OpenaiCompatible,
    Anthropic,
}

impl std::fmt::Display for ChannelProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenaiCompatible => write!(f, "openai-compatible"),
            Self::Anthropic => write!(f, "anthropic"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CostTier {
    Free,
    Paid,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Active,
    Cooldown,
    Disabled,
    QuotaExhausted,
}

impl KeyStatus {
    pub fn is_available(&self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl Usage {
    pub fn add(&mut self, other: &Self) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub id: i64,
    pub name: String,
    pub provider: ChannelProvider,
    pub base_url: String,
    /// Comma-separated list of user groups allowed to use this channel
    /// (e.g. "default,vip"). A user in any listed group can route here.
    #[serde(default = "default_group")]
    pub group: String,
}

fn default_group() -> String {
    "default".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyConfig {
    pub id: i64,
    pub channel_id: i64,
    pub api_key: String,
    pub label: Option<String>,
    pub cost_tier: CostTier,
    pub key_priority: i32,
    pub price_per_1k_tokens: Option<f64>,
    pub free_quota: Option<u64>,
    pub used_quota: u64,
    pub quota_reset_period: Option<String>,
    pub status: KeyStatus,
}

#[derive(Debug, Clone)]
pub struct ModelBinding {
    pub model_id: i64,
    pub canonical_name: String,
    pub channel_id: i64,
    pub upstream_model_name: String,
    /// 该绑定的调用优先级（数字越小越优先）。
    pub priority: i32,
    /// 该绑定的负载均衡权重（>=1，仅 load_balance 策略生效）。
    pub weight: i32,
}

/// 计费模式。
///
/// JSON 序列化为变体名字符串（`"Token"` / `"PerCall"` / `"Expression"`），
/// 与前端 `pricing.ts` 的字符串字面量类型一致。数据库存储仍走 `as_i32` / `from_i32`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[repr(i32)]
pub enum BillingType {
    /// 按 token：input_price / output_price（元/1K tokens）
    #[default]
    Token = 0,
    /// 按调用次数：call_price（元/次）
    PerCall = 1,
    /// 分段表达式：billing_expr（结果单位为元）
    Expression = 2,
}

impl From<BillingType> for i32 {
    fn from(v: BillingType) -> i32 {
        v as i32
    }
}

impl From<i32> for BillingType {
    fn from(v: i32) -> Self {
        Self::from_i32(v)
    }
}

impl BillingType {
    pub fn from_i32(v: i32) -> Self {
        match v {
            1 => Self::PerCall,
            2 => Self::Expression,
            _ => Self::Token,
        }
    }
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// 渠道-模型级定价（绑定在 `model_channels` 表上）。
///
/// 同一模型在不同渠道可有不同的定价。未配置（`is_configured()` 为 false）
/// 表示该绑定免费（cost = 0）。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelModelPricing {
    pub billing_type: BillingType,
    /// 按 token 模式使用（元/1K tokens）
    pub input_price: f64,
    /// 按 token 模式使用（元/1K tokens）
    pub output_price: f64,
    /// 按调用次数模式使用（元/次）
    pub call_price: f64,
    /// 分段表达式模式使用（evalexpr 语法，结果单位为元）
    pub billing_expr: Option<String>,
}

impl ChannelModelPricing {
    /// 该定价是否已配置（非默认全零/空）。
    pub fn is_configured(&self) -> bool {
        match self.billing_type {
            BillingType::Token => self.input_price > 0.0 || self.output_price > 0.0,
            BillingType::PerCall => self.call_price > 0.0,
            BillingType::Expression => self
                .billing_expr
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false),
        }
    }
}

/// Per-token usage statistics for the admin panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageStats {
    pub total_tokens: i64,
    pub request_count: i64,
    pub last_used_at: Option<String>,
}

/// Result of testing a channel or model connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectionTestResult {
    pub success: bool,
    pub latency_ms: u64,
    pub error: Option<String>,
}

/// A model entry returned from the channel-models endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelModelEntry {
    pub model_id: Option<i64>,
    pub canonical_name: String,
    pub upstream_model_name: Option<String>,
    /// 该绑定的调用优先级（数字越小越优先）。
    #[serde(default = "default_binding_priority")]
    pub priority: i32,
    /// 该绑定的负载均衡权重（>=1，仅 load_balance 策略生效）。
    #[serde(default = "default_binding_weight")]
    pub weight: i32,
    /// 该渠道-模型绑定的定价配置。
    #[serde(default)]
    pub pricing: ChannelModelPricing,
}

fn default_binding_priority() -> i32 {
    100
}

fn default_binding_weight() -> i32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UserConfig {
    pub id: i64,
    pub username: String,
    /// 1=common, 10=admin, 100=root
    pub role: i32,
    /// 1=enabled, 2=disabled
    pub status: i32,
    /// total quota (bank account)
    pub quota: i64,
    pub used_quota: i64,
    /// user group for channel routing
    #[serde(rename = "group")]
    pub group: String,
}

impl UserConfig {
    pub fn is_enabled(&self) -> bool {
        self.status == 1
    }
    pub fn is_admin(&self) -> bool {
        self.role >= 10
    }
    pub fn remaining_quota(&self) -> i64 {
        self.quota - self.used_quota
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenConfig {
    pub id: i64,
    pub user_id: i64,
    pub key: String,
    pub name: Option<String>,
    pub remain_quota: i64,
    pub used_quota: i64,
    pub unlimited_quota: bool,
    /// unix timestamp, -1=never
    pub expired_time: i64,
    pub model_limits_enabled: bool,
    pub model_limits: Option<Vec<String>>,
    /// 1=enabled, 2=disabled, 3=exhausted
    pub status: i32,
    pub allow_ips: Option<Vec<String>>,
}

impl TokenConfig {
    pub fn is_enabled(&self) -> bool {
        self.status == 1
    }
    pub fn is_expired(&self, now: i64) -> bool {
        self.expired_time != -1 && self.expired_time <= now
    }
    pub fn allows_model(&self, model: &str) -> bool {
        if !self.model_limits_enabled {
            return true;
        }
        match &self.model_limits {
            Some(models) => models.iter().any(|m| m == model),
            None => true,
        }
    }
    pub fn allows_ip(&self, client_ip: &str) -> bool {
        match &self.allow_ips {
            None => true,
            Some(ips) => ips.iter().any(|ip| ip == client_ip),
        }
    }
}

// ===== Admin API response types =====

/// Dashboard overview statistics for the admin panel.
///
/// Aggregates today's activity: total tokens consumed, total API requests,
/// error count, and the number of available (active) upstream keys.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardOverview {
    /// Total tokens consumed today (from `usage_logs`).
    pub today_tokens: i64,
    /// Total API requests today (from `request_logs`).
    pub today_requests: i64,
    /// Number of requests that returned an error status (>= 400) today.
    pub today_errors: i64,
    /// Number of upstream keys with `status = 'active'`.
    pub available_keys: i64,
}

/// Per-model usage statistics for the admin dashboard.
///
/// Represents the top N models by total token consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    /// Model canonical name (resolved from `models.canonical_name`).
    pub model: String,
    /// Sum of `total_tokens` across all usage logs for this model.
    pub total_tokens: i64,
    /// Number of usage log entries for this model.
    pub request_count: i64,
    /// Sum of `quota_cost` for this model.
    pub total_cost: i64,
}

/// A single request log entry for the admin logs view.
///
/// Maps to a row in the `request_logs` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestLog {
    pub id: i64,
    pub request_id: String,
    pub client_ip: Option<String>,
    pub method: String,
    pub path: String,
    pub client_model: Option<String>,
    pub normalized_model: Option<String>,
    pub channel_name: Option<String>,
    pub key_label: Option<String>,
    pub upstream_status: Option<i32>,
    /// 实际调用的上游模型名（绑定时配置的 upstream_model_name）。
    /// 与归一化后的 `normalized_model` 区分开，便于审计定位「具体调
    /// 用了哪个上游模型」。
    pub upstream_model: Option<String>,
    pub response_status: i32,
    pub duration_ms: i64,
    pub stream: bool,
    pub user_id: Option<i64>,
    pub token_id: Option<i64>,
    pub quota_cost: i64,
    pub error_message: Option<String>,
    /// RFC 3339 timestamp with `+08:00` offset, e.g. `"2026-07-01T12:34:56+08:00"`.
    /// Prior to migrate_v4_to_v5, this was a SQLite `datetime('now')` UTC text
    /// like `"2026-07-01 04:34:56"`; old rows are converted on migration.
    pub created_at: String,
}

/// Aggregated usage summary grouped by channel and model.
///
/// Used by the admin usage report view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageSummary {
    pub channel_id: i64,
    pub channel_name: String,
    /// Model canonical name resolved via `usage_logs.model_id`.
    pub model: String,
    pub total_tokens: i64,
    pub request_count: i64,
    pub total_cost: i64,
}

/// Admin session authentication context.
///
/// Injected into request extensions by `session_middleware`.
/// Unlike `AuthContext` (which carries both user + token for proxy auth),
/// this only carries the user — admin panel auth is cookie-based, not
/// Bearer-token-based.
#[derive(Debug, Clone)]
pub struct AdminAuthContext {
    pub user: UserConfig,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user: UserConfig,
    pub token: TokenConfig,
    /// 客户端 IP（由 auth 中间件从 x-forwarded-for / x-real-ip 提取）。
    /// 用于 request_logs 审计记录。
    pub client_ip: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_status_available() {
        assert!(KeyStatus::Active.is_available());
        assert!(!KeyStatus::Cooldown.is_available());
    }
    #[test]
    fn test_usage_add() {
        let mut a = Usage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 };
        a.add(&Usage { prompt_tokens: 20, completion_tokens: 10, total_tokens: 30 });
        assert_eq!(a.total_tokens, 45);
    }
    #[test]
    fn test_provider_display() {
        assert_eq!(ChannelProvider::OpenaiCompatible.to_string(), "openai-compatible");
    }

    #[test]
    fn test_user_config_serialization() {
        let u = UserConfig {
            id: 1,
            username: "alice".into(),
            role: 10,
            status: 1,
            quota: 1000,
            used_quota: 200,
            group: "default".into(),
        };
        let s = serde_json::to_string(&u).unwrap();
        assert!(s.contains("\"group\":\"default\""), "group field must serialize as 'group': {}", s);
        let back: UserConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back, u);
    }

    #[test]
    fn test_user_config_helpers() {
        let admin = UserConfig {
            id: 1, username: "a".into(), role: 100, status: 1,
            quota: 100, used_quota: 30, group: "default".into(),
        };
        assert!(admin.is_admin());
        assert!(admin.is_enabled());
        assert_eq!(admin.remaining_quota(), 70);

        let disabled = UserConfig { role: 1, status: 2, ..admin };
        assert!(!disabled.is_admin());
        assert!(!disabled.is_enabled());
    }

    #[test]
    fn test_token_config_serialization() {
        let t = TokenConfig {
            id: 1,
            user_id: 1,
            key: "sk-token".into(),
            name: Some("my token".into()),
            remain_quota: 500,
            used_quota: 100,
            unlimited_quota: false,
            expired_time: -1,
            model_limits_enabled: true,
            model_limits: Some(vec!["gpt-4".into(), "claude".into()]),
            status: 1,
            allow_ips: Some(vec!["127.0.0.1".into()]),
        };
        let s = serde_json::to_string(&t).unwrap();
        let back: TokenConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back, t);
    }

    #[test]
    fn test_token_config_helpers() {
        let mut t = TokenConfig {
            id: 1, user_id: 1, key: "k".into(), name: None,
            remain_quota: 0, used_quota: 0, unlimited_quota: false,
            expired_time: -1, model_limits_enabled: false,
            model_limits: None, status: 1, allow_ips: None,
        };
        // never expires
        assert!(!t.is_expired(9_999_999_999));
        // expires in past
        t.expired_time = 1;
        assert!(t.is_expired(2));
        assert!(!t.is_expired(0));

        // model limits disabled → all allowed
        assert!(t.allows_model("anything"));

        // enable limits
        t.model_limits_enabled = true;
        t.model_limits = Some(vec!["gpt-4".into()]);
        assert!(t.allows_model("gpt-4"));
        assert!(!t.allows_model("claude"));

        // empty limits list when enabled → nothing allowed
        t.model_limits = Some(vec![]);
        assert!(!t.allows_model("gpt-4"));

        // ip whitelist
        t.allow_ips = Some(vec!["10.0.0.1".into(), "10.0.0.2".into()]);
        assert!(t.allows_ip("10.0.0.1"));
        assert!(!t.allows_ip("10.0.0.3"));

        // no whitelist → allow all
        t.allow_ips = None;
        assert!(t.allows_ip("anything"));
    }
}
