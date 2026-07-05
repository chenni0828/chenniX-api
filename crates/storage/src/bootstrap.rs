use chennix_common::{ChannelProvider, CostTier, ProxyError, ProxyResult};
use rusqlite::Connection;
use serde::Deserialize;
use crate::channels::{ChannelRepo, DiscoveredModelRepo};
use crate::keys::KeyRepo;
use crate::models::ModelRepo;

#[derive(Debug, Deserialize)]
struct BootstrapConfig {
    #[serde(default)]
    models: Vec<ModelEntry>,
    #[serde(default)]
    channels: Vec<ChannelEntry>,
    #[serde(default)]
    keys: Vec<KeyEntry>,
    #[serde(default)]
    bindings: Vec<BindingEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelEntry {
    canonical_name: String,
}

#[derive(Debug, Deserialize)]
struct ChannelEntry {
    name: String,
    provider: String,
    base_url: String,
}

#[derive(Debug, Deserialize)]
struct KeyEntry {
    channel: String,
    api_key: String,
    label: Option<String>,
    #[serde(default = "default_cost_tier")]
    cost_tier: String,
    #[serde(default = "default_key_priority")]
    key_priority: i32,
    price_per_1k_tokens: Option<f64>,
    free_quota: Option<u64>,
    quota_reset_period: Option<String>,
}
fn default_cost_tier() -> String { "paid".into() }
fn default_key_priority() -> i32 { 100 }

#[derive(Debug, Deserialize)]
struct BindingEntry {
    model: String,
    channel: String,
    upstream_model_name: String,
    #[serde(default = "default_binding_priority")]
    priority: i32,
}
fn default_binding_priority() -> i32 { 100 }

pub fn import_from_yaml(conn: &Connection, yaml_path: &str) -> ProxyResult<()> {
    let content = std::fs::read_to_string(yaml_path)
        .map_err(|e| ProxyError::Config(format!("read yaml {}: {}", yaml_path, e)))?;
    let config: BootstrapConfig = serde_yaml::from_str(&content)
        .map_err(|e| ProxyError::Config(format!("parse yaml: {}", e)))?;

    let tx = conn
        .unchecked_transaction()
        .map_err(|e| ProxyError::Storage(e.to_string()))?;

    // 1. 导入模型
    let model_repo = ModelRepo::new(&tx);
    for m in &config.models {
        model_repo.create_model(&m.canonical_name)?;
    }

    // 2. 导入渠道
    let ch_repo = ChannelRepo::new(&tx);
    for c in &config.channels {
        let provider = match c.provider.as_str() {
            "anthropic" => ChannelProvider::Anthropic,
            _ => ChannelProvider::OpenaiCompatible,
        };
        ch_repo.create_channel(&c.name, &provider, &c.base_url)?;
    }

    // 3. 导入 Key
    let key_repo = KeyRepo::new(&tx);
    for k in &config.keys {
        let channel = ch_repo
            .get_channel_by_name(&k.channel)?
            .ok_or_else(|| ProxyError::Config(format!("channel not found: {}", k.channel)))?;
        let cost_tier = match k.cost_tier.as_str() {
            "free" => CostTier::Free,
            _ => CostTier::Paid,
        };
        key_repo.create_key(
            channel.id,
            &k.api_key,
            k.label.as_deref(),
            cost_tier,
            k.key_priority,
            k.price_per_1k_tokens,
            k.free_quota,
            k.quota_reset_period.as_deref(),
        )?;
    }

    // 4. 导入绑定（同时写入 discovered_models，确保小模型池有数据）
    let dm_repo = DiscoveredModelRepo::new(&tx);
    for b in &config.bindings {
        let model = model_repo
            .get_model_by_name(&b.model)?
            .ok_or_else(|| ProxyError::Config(format!("model not found: {}", b.model)))?;
        let channel = ch_repo
            .get_channel_by_name(&b.channel)?
            .ok_or_else(|| ProxyError::Config(format!("channel not found: {}", b.channel)))?;
        model_repo.add_binding(model.0, channel.id, &b.upstream_model_name, b.priority)?;
        dm_repo.upsert_discovered_model(
            channel.id,
            &b.upstream_model_name,
            false,
            Some("bootstrap"),
            None,
        )?;
    }

    tx.commit().map_err(|e| ProxyError::Storage(e.to_string()))?;

    // 检测占位符密钥并输出醒目 WARNING。
    // 新手常直接用 bootstrap.example.yaml 的占位符部署，看到面板有模型
    // 就以为跑通了，结果一请求就 401。这里主动告警，缩短排查路径。
    warn_on_placeholder_keys(&config.keys);

    Ok(())
}

/// 扫描 bootstrap 导入的 Key 列表，发现疑似占位符就输出 WARNING。
///
/// 判定规则（大小写不敏感）：
/// - 精确匹配 `bootstrap.example.yaml` 中出现的占位符：`sk-your-openai-key-here`、
///   `sk-ant-your-anthropic-key-here`、`sk-your-deepseek-key-here`
/// - 包含 `your-` / `-here` / `change-me` / `example` 等典型占位词
///
/// 仅记日志，不阻断启动 —— 用户可能确实想用占位符先跑通流程。
fn warn_on_placeholder_keys(keys: &[KeyEntry]) {
    const KNOWN_PLACEHOLDERS: &[&str] = &[
        "sk-your-openai-key-here",
        "sk-ant-your-anthropic-key-here",
        "sk-your-deepseek-key-here",
    ];
    const PLACEHOLDER_MARKERS: &[&str] = &[
        "your-",
        "-here",
        "change-me",
        "example",
        "placeholder",
        "xxxx",
    ];

    let mut found: Vec<(&str, &str)> = Vec::new();
    for k in keys {
        let lower = k.api_key.to_lowercase();
        let is_placeholder = KNOWN_PLACEHOLDERS.iter().any(|p| lower == *p)
            || PLACEHOLDER_MARKERS.iter().any(|m| lower.contains(m));
        if is_placeholder {
            let label = k.label.as_deref().unwrap_or("(no label)");
            found.push((label, &k.api_key));
        }
    }

    if !found.is_empty() {
        tracing::warn!("");
        tracing::warn!("┌─────────────────────────────────────────────────────────────┐");
        tracing::warn!("│  ⚠️  检测到 {} 个疑似占位符 API Key：", found.len());
        for (label, key) in &found {
            // 只显示前 8 + 后 4 字符，避免完整泄漏到日志。
            // 用 chars 收集避免非 ASCII 切片 panic（虽然 API key 一般是 ASCII）。
            let chars: Vec<char> = key.chars().collect();
            let masked = if chars.len() > 12 {
                let head: String = chars.iter().take(8).collect();
                let tail: String = chars[chars.len()-4..].iter().collect();
                format!("{}...{}", head, tail)
            } else {
                key.to_string()
            };
            tracing::warn!("│    • [{}] {}", label, masked);
        }
        tracing::warn!("│");
        tracing::warn!("│  这些 Key 大概率来自 bootstrap.example.yaml 的占位符。");
        tracing::warn!("│  请登录管理面板 → Channels → 修改为真实 API Key，");
        tracing::warn!("│  否则所有请求都会因 401 失败。");
        tracing::warn!("└─────────────────────────────────────────────────────────────┘");
        tracing::warn!("");
    }
}

/// 检查 SQLite 是否为空 (无渠道配置), 用于判断是否需要引导导入
pub fn is_db_empty(conn: &Connection) -> ProxyResult<bool> {
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM channels", [], |r| r.get(0))
        .map_err(|e| ProxyError::Storage(e.to_string()))?;
    Ok(count == 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::init_db;

    #[test]
    fn test_import_from_yaml() {
        let yaml = r#"
models:
  - canonical_name: "deepseek-v3"
channels:
  - name: "deepseek-official"
    provider: "openai-compatible"
    base_url: "https://api.deepseek.com/v1"
keys:
  - channel: "deepseek-official"
    api_key: "sk-xxx"
    label: "主账号"
    cost_tier: "paid"
    key_priority: 1
    price_per_1k_tokens: 0.001
bindings:
  - model: "deepseek-v3"
    channel: "deepseek-official"
    upstream_model_name: "deepseek-chat"
    priority: 10
"#;
        let tmp = std::env::temp_dir().join("chennix_test_bootstrap.yaml");
        std::fs::write(&tmp, yaml).unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        assert!(is_db_empty(&conn).unwrap());
        import_from_yaml(&conn, tmp.to_str().unwrap()).unwrap();
        assert!(!is_db_empty(&conn).unwrap());

        let model_repo = ModelRepo::new(&conn);
        let model = model_repo.get_model_by_name("deepseek-v3").unwrap();
        assert_eq!(model.unwrap().1, "deepseek-v3");
        let bindings = model_repo.get_bindings_for_model(1).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].upstream_model_name, "deepseek-chat");
        assert_eq!(bindings[0].priority, 10);
    }

    /// 占位符检测不应误报真实 key（如 `sk-xxx` 这种短测试 key）。
    /// 仅当含 `your-` / `-here` / `change-me` / `example` / `placeholder`
    /// / `xxxx` 或精确匹配已知占位符时才告警。
    #[test]
    fn test_warn_on_placeholder_keys_does_not_false_positive() {
        let keys = vec![KeyEntry {
            channel: "test".into(),
            api_key: "sk-xxx".into(), // 3 个 x，不含 4 个 x 的 "xxxx"
            label: Some("test".into()),
            cost_tier: "paid".into(),
            key_priority: 100,
            price_per_1k_tokens: None,
            free_quota: None,
            quota_reset_period: None,
        }];
        // 仅验证不 panic；告警逻辑只记日志不返回值。
        warn_on_placeholder_keys(&keys);
    }

    /// 占位符检测应识别 example.yaml 中的占位符。
    #[test]
    fn test_warn_on_placeholder_keys_detects_known_placeholders() {
        let keys = vec![
            KeyEntry {
                channel: "openai".into(),
                api_key: "sk-your-openai-key-here".into(),
                label: Some("主账号".into()),
                cost_tier: "paid".into(),
                key_priority: 0,
                price_per_1k_tokens: Some(0.01),
                free_quota: None,
                quota_reset_period: None,
            },
            KeyEntry {
                channel: "anthropic".into(),
                api_key: "sk-ant-your-anthropic-key-here".into(),
                label: None,
                cost_tier: "paid".into(),
                key_priority: 0,
                price_per_1k_tokens: Some(0.015),
                free_quota: None,
                quota_reset_period: None,
            },
        ];
        // 仅验证不 panic；视觉检查日志应输出 WARNING 框。
        warn_on_placeholder_keys(&keys);
    }
}
