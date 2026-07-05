use chennix_common::{ChannelConfig, ChannelModelEntry, ChannelProvider, ProxyError, ProxyResult};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

use crate::now_iso8601;

pub struct ChannelRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ChannelRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create_channel(
        &self,
        name: &str,
        provider: &ChannelProvider,
        base_url: &str,
    ) -> ProxyResult<i64> {
        let now = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO channels (name, provider, base_url, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)",
                params![name, provider.to_string(), base_url, now],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_channel_by_id(&self, id: i64) -> ProxyResult<Option<ChannelConfig>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, provider, base_url, \"group\" FROM channels WHERE id = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<ChannelConfig> = stmt
            .query_row(params![id], map_channel_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn get_channel_by_name(&self, name: &str) -> ProxyResult<Option<ChannelConfig>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, provider, base_url, \"group\" FROM channels WHERE name = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<ChannelConfig> = stmt
            .query_row(params![name], map_channel_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn list_channels(&self) -> ProxyResult<Vec<ChannelConfig>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, name, provider, base_url, \"group\" FROM channels ORDER BY id")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], map_channel_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    pub fn delete_channel(&self, id: i64) -> ProxyResult<()> {
        self.conn
            .execute("DELETE FROM channels WHERE id = ?1", params![id])
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Update the `group` (comma-separated user-group whitelist) of a channel.
    pub fn update_group(&self, id: i64, group: &str) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE channels SET \"group\" = ?1, updated_at = ?2 WHERE id = ?3",
                params![group, now_iso8601(), id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    // ===== Admin API methods =====

    /// Create a channel with full control over all configurable fields.
    ///
    /// This is the admin-panel variant of `create_channel` — it accepts the
    /// `group` field in addition to the base parameters.
    pub fn create_channel_full(
        &self,
        name: &str,
        provider: &ChannelProvider,
        base_url: &str,
        group: &str,
    ) -> ProxyResult<i64> {
        let now = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO channels (name, provider, base_url, \"group\", created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![name, provider.to_string(), base_url, group, now],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a channel's configurable fields.
    ///
    /// All of `name`, `provider`, `base_url`, and `group` are set
    /// to the provided values. `created_at` is not touched.
    pub fn update_channel(
        &self,
        id: i64,
        name: &str,
        provider: &ChannelProvider,
        base_url: &str,
        group: &str,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE channels
                 SET name = ?1, provider = ?2, base_url = ?3, \"group\" = ?4,
                     updated_at = ?5
                 WHERE id = ?6",
                params![name, provider.to_string(), base_url, group, now_iso8601(), id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Get all models associated with a channel via the `model_channels` table.
    ///
    /// Returns entries with model_id, canonical_name, upstream_model_name,
    /// per-binding priority, per-binding weight, and per-binding pricing.
    pub fn get_channel_models(&self, channel_id: i64) -> ProxyResult<Vec<ChannelModelEntry>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT mc.model_id, m.canonical_name, mc.upstream_model_name,
                        mc.billing_type, mc.input_price, mc.output_price,
                        mc.call_price, mc.billing_expr, mc.priority, mc.weight
                 FROM model_channels mc
                 JOIN models m ON mc.model_id = m.id
                 WHERE mc.channel_id = ?1
                 ORDER BY m.canonical_name",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![channel_id], |r| {
                let billing_type = r.get::<_, i32>(3).unwrap_or(0);
                let billing_expr: Option<String> = r.get(7).ok();
                Ok(ChannelModelEntry {
                    model_id: r.get(0)?,
                    canonical_name: r.get(1)?,
                    upstream_model_name: r.get(2)?,
                    priority: r.get(8).unwrap_or(100),
                    weight: r.get(9).unwrap_or(1),
                    pricing: chennix_common::ChannelModelPricing {
                        billing_type: chennix_common::BillingType::from_i32(billing_type),
                        input_price: r.get(4).unwrap_or(0.0),
                        output_price: r.get(5).unwrap_or(0.0),
                        call_price: r.get(6).unwrap_or(0.0),
                        billing_expr,
                    },
                })
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }
}

fn map_channel_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelConfig> {
    let provider_str: String = r.get(2)?;
    let provider = match provider_str.as_str() {
        "openai-compatible" => ChannelProvider::OpenaiCompatible,
        "anthropic" => ChannelProvider::Anthropic,
        _ => ChannelProvider::OpenaiCompatible,
    };
    Ok(ChannelConfig {
        id: r.get(0)?,
        name: r.get(1)?,
        provider,
        base_url: r.get(3)?,
        group: r.get(4)?,
    })
}

/// A discovered upstream model row (`discovered_models` table).
///
/// 额度字段（`quota_limit`/`quota_unit`/`quota_window`/`used_quota`/
/// `last_reset_at`/`quota_status`）归属以 `(channel_id, raw_model_name)` 为
/// 标识，即小模型二元组本身。`quota_limit` 为 `None` 表示无限制。
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredModel {
    pub id: i64,
    pub channel_id: i64,
    pub raw_model_name: String,
    pub discovered_at: String,
    pub status: String,
    pub merged_to_model_id: Option<i64>,
    pub is_free: bool,
    pub source: Option<String>,
    pub metadata: Option<String>,
    /// `None` 表示无限制。
    pub quota_limit: Option<i64>,
    /// `'token'` | `'call'` | `None`。
    pub quota_unit: Option<String>,
    /// `'day'` | `'month'` | `'total'` | `None`。
    pub quota_window: Option<String>,
    pub used_quota: i64,
    pub last_reset_at: Option<String>,
    /// `'available'` | `'exhausted'`。
    pub quota_status: String,
}

/// 一个已发现小模型及其被大模型绑定的次数（用于模型管理页"小模型池"）。
///
/// `binding_count` = 该 `(channel_id, raw_model_name)` 在 `model_channels`
/// 中作为 `(channel_id, upstream_model_name)` 出现的次数。
#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredModelWithCount {
    #[serde(flatten)]
    pub model: DiscoveredModel,
    pub binding_count: i64,
}

pub struct DiscoveredModelRepo<'a> {
    conn: &'a Connection,
}

impl<'a> DiscoveredModelRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// 重新发现时的小模型 upsert。
    ///
    /// **关键逻辑**：若 `(channel_id, raw_model_name)` 已存在，只刷新发现
    /// 相关字段（`discovered_at`/`is_free`/`source`/`metadata`），**保留**
    /// 原有额度数据（`quota_limit`/`quota_unit`/`quota_window`/`used_quota`/
    /// `last_reset_at`/`quota_status` 不被重置）；若不存在则新建一行空额度
    /// 行（额度字段走列默认值：`quota_limit=NULL`、`used_quota=0`、
    /// `quota_status='available'`）。
    pub fn upsert_discovered_model(
        &self,
        channel_id: i64,
        raw_model_name: &str,
        is_free: bool,
        source: Option<&str>,
        metadata: Option<&str>,
    ) -> ProxyResult<()> {
        let now = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO discovered_models
                    (channel_id, raw_model_name, is_free, source, metadata, discovered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(channel_id, raw_model_name) DO UPDATE SET
                    discovered_at = ?6,
                    is_free = excluded.is_free,
                    source = excluded.source,
                    metadata = excluded.metadata",
                params![
                    channel_id,
                    raw_model_name,
                    is_free as i32,
                    source,
                    metadata,
                    now,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 列出全部已发现小模型（用于模型页"小模型池"）。
    pub fn list_discovered_models(&self) -> ProxyResult<Vec<DiscoveredModel>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, channel_id, raw_model_name, discovered_at, status,
                        merged_to_model_id, is_free, source, metadata,
                        quota_limit, quota_unit, quota_window, used_quota,
                        last_reset_at, quota_status
                 FROM discovered_models
                 ORDER BY channel_id, raw_model_name",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], map_discovered_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// 列出全部已发现小模型，并附带每个小模型被大模型绑定的次数
    /// （`binding_count` = `model_channels` 中 `(channel_id,
    /// upstream_model_name)` 等于 `(channel_id, raw_model_name)` 的行数）。
    pub fn list_discovered_models_with_binding_count(
        &self,
    ) -> ProxyResult<Vec<DiscoveredModelWithCount>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT dm.id, dm.channel_id, dm.raw_model_name, dm.discovered_at, dm.status,
                        dm.merged_to_model_id, dm.is_free, dm.source, dm.metadata,
                        dm.quota_limit, dm.quota_unit, dm.quota_window, dm.used_quota,
                        dm.last_reset_at, dm.quota_status,
                        COUNT(mc.model_id) AS binding_count
                 FROM discovered_models dm
                 LEFT JOIN model_channels mc
                   ON mc.channel_id = dm.channel_id
                  AND mc.upstream_model_name = dm.raw_model_name
                 GROUP BY dm.id
                 ORDER BY dm.channel_id, dm.raw_model_name",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let model = map_discovered_row(r)?;
                let binding_count = r.get(15).unwrap_or(0);
                Ok(DiscoveredModelWithCount {
                    model,
                    binding_count,
                })
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// 列出某个渠道下的全部已发现小模型。
    pub fn list_discovered_models_for_channel(
        &self,
        channel_id: i64,
    ) -> ProxyResult<Vec<DiscoveredModel>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, channel_id, raw_model_name, discovered_at, status,
                        merged_to_model_id, is_free, source, metadata,
                        quota_limit, quota_unit, quota_window, used_quota,
                        last_reset_at, quota_status
                 FROM discovered_models
                 WHERE channel_id = ?1
                 ORDER BY raw_model_name",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![channel_id], map_discovered_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// 读取某个 `(channel_id, raw_model_name)` 的小模型行；不存在返回 `None`。
    pub fn get_discovered_model(
        &self,
        channel_id: i64,
        raw_model_name: &str,
    ) -> ProxyResult<Option<DiscoveredModel>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, channel_id, raw_model_name, discovered_at, status,
                        merged_to_model_id, is_free, source, metadata,
                        quota_limit, quota_unit, quota_window, used_quota,
                        last_reset_at, quota_status
                 FROM discovered_models
                 WHERE channel_id = ?1 AND raw_model_name = ?2",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<DiscoveredModel> = stmt
            .query_row(params![channel_id, raw_model_name], map_discovered_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    /// 设置某个小模型的额度配置。会重置 `used_quota=0`、
    /// `quota_status='available'`、`last_reset_at=now`。传入 `quota_limit=None`
    /// 表示无限制。
    pub fn update_discovered_model_quota(
        &self,
        channel_id: i64,
        raw_model_name: &str,
        quota_limit: Option<i64>,
        quota_unit: Option<&str>,
        quota_window: Option<&str>,
    ) -> ProxyResult<()> {
        let now = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO discovered_models
                    (channel_id, raw_model_name, quota_limit, quota_unit, quota_window,
                     used_quota, quota_status, last_reset_at, discovered_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, 0, 'available', ?6, ?6)
                 ON CONFLICT(channel_id, raw_model_name) DO UPDATE SET
                    quota_limit = excluded.quota_limit,
                    quota_unit = excluded.quota_unit,
                    quota_window = excluded.quota_window,
                    used_quota = 0,
                    quota_status = 'available',
                    last_reset_at = ?6",
                params![channel_id, raw_model_name, quota_limit, quota_unit, quota_window, now],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 手动重置某个小模型的额度（用于 `total` 周期已耗尽后的手动恢复）。
    /// `used_quota=0`、`quota_status='available'`、`last_reset_at=now`。
    pub fn reset_discovered_model_quota(
        &self,
        channel_id: i64,
        raw_model_name: &str,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE discovered_models
                 SET used_quota = 0,
                     quota_status = 'available',
                     last_reset_at = ?1
                 WHERE channel_id = ?2 AND raw_model_name = ?3",
                params![now_iso8601(), channel_id, raw_model_name],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 累加某个小模型的已用额度（请求成功后由 tracker 调用）。
    ///
    /// 原子地执行 `used_quota = used_quota + delta` 并写入新的
    /// `quota_status`。`quota_status` 取 `discovered_models` 列存储的字符串
    /// 值（`'available'` / `'exhausted'`）。若行不存在则静默返回 `Ok(())`
    /// —— quota 累加是 best-effort，不应让请求失败。
    pub fn add_discovered_model_usage(
        &self,
        channel_id: i64,
        raw_model_name: &str,
        delta: i64,
        quota_status: &str,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE discovered_models
                 SET used_quota = used_quota + ?1,
                     quota_status = ?2
                 WHERE channel_id = ?3 AND raw_model_name = ?4",
                params![delta, quota_status, channel_id, raw_model_name],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 删除某个渠道下的指定发现模型（从小模型池移除）。
    ///
    /// 不会删除 `model_channels` 中引用该 `(channel_id, upstream_model_name)`
    /// 的绑定行——调用方应在 UI 层检查 `binding_count == 0` 后再调用。
    pub fn delete_discovered_model(
        &self,
        channel_id: i64,
        raw_model_name: &str,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "DELETE FROM discovered_models
                 WHERE channel_id = ?1 AND raw_model_name = ?2",
                params![channel_id, raw_model_name],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }
}

fn map_discovered_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<DiscoveredModel> {
    Ok(DiscoveredModel {
        id: r.get(0)?,
        channel_id: r.get(1)?,
        raw_model_name: r.get(2)?,
        discovered_at: r.get(3)?,
        status: r.get(4)?,
        merged_to_model_id: r.get(5)?,
        is_free: r.get::<_, i32>(6)? != 0,
        source: r.get(7)?,
        metadata: r.get(8)?,
        quota_limit: r.get(9)?,
        quota_unit: r.get(10)?,
        quota_window: r.get(11)?,
        used_quota: r.get(12).unwrap_or(0),
        last_reset_at: r.get(13)?,
        quota_status: r.get(14).unwrap_or_else(|_| "available".to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::init_db;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_channel_crud() {
        let conn = setup();
        let repo = ChannelRepo::new(&conn);
        let id = repo
            .create_channel("test", &ChannelProvider::OpenaiCompatible, "http://test")
            .unwrap();
        let ch = repo.get_channel_by_id(id).unwrap().unwrap();
        assert_eq!(ch.name, "test");
        assert_eq!(ch.provider, ChannelProvider::OpenaiCompatible);
        // group column has DB default 'default'
        assert_eq!(ch.group, "default");
        let channels = repo.list_channels().unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].group, "default");
        // update group
        repo.update_group(id, "default,vip").unwrap();
        let ch = repo.get_channel_by_id(id).unwrap().unwrap();
        assert_eq!(ch.group, "default,vip");
        repo.delete_channel(id).unwrap();
        assert!(repo.get_channel_by_id(id).unwrap().is_none());
    }

    #[test]
    fn test_get_channel_by_name_missing_returns_none() {
        let conn = setup();
        let repo = ChannelRepo::new(&conn);
        assert!(repo.get_channel_by_name("nope").unwrap().is_none());
        assert!(repo.get_channel_by_id(99999).unwrap().is_none());
    }

    #[test]
    fn test_real_db_error_surfaces_as_storage_error() {
        let conn = setup();
        let repo = ChannelRepo::new(&conn);
        conn.execute("DROP TABLE channels", []).unwrap();
        let result = repo.get_channel_by_id(1);
        assert!(result.is_err(), "expected Err, got {:?}", result);
        match result.unwrap_err() {
            ProxyError::Storage(_) => {}
            other => panic!("expected ProxyError::Storage, got {:?}", other),
        }
    }

    #[test]
    fn test_get_channel_models() {
        let conn = setup();
        // Create a channel first
        let repo = ChannelRepo::new(&conn);
        let ch_id = repo
            .create_channel("test-ch", &ChannelProvider::OpenaiCompatible, "http://test")
            .unwrap();

        // Create a model and bind it to the channel
        let model_repo = crate::models::ModelRepo::new(&conn);
        let model_id = model_repo.create_model("gpt-4").unwrap();
        model_repo.add_binding(model_id, ch_id, "gpt-4-upstream", 100).unwrap();

        let models = repo.get_channel_models(ch_id).unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].canonical_name, "gpt-4");
        assert_eq!(models[0].model_id, Some(model_id));
        // weight defaults to 1 for new bindings.
        assert_eq!(models[0].weight, 1);

        // Empty for non-existent channel
        let empty = repo.get_channel_models(999).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_upsert_discovered_model_preserves_quota() {
        let conn = setup();
        conn.execute(
            "INSERT INTO channels (id, name, provider, base_url) VALUES (1, 'ch', 'openai-compatible', 'http://x')",
            [],
        )
        .unwrap();
        let repo = DiscoveredModelRepo::new(&conn);

        // First discovery: creates an empty-quota row.
        repo.upsert_discovered_model(1, "gpt-4o", false, Some("list"), None)
            .unwrap();
        let m = repo.get_discovered_model(1, "gpt-4o").unwrap().unwrap();
        assert_eq!(m.quota_limit, None);
        assert_eq!(m.used_quota, 0);
        assert_eq!(m.quota_status, "available");
        assert!(!m.is_free);

        // Configure a quota.
        repo.update_discovered_model_quota(1, "gpt-4o", Some(1_000_000), Some("token"), Some("month"))
            .unwrap();
        let m = repo.get_discovered_model(1, "gpt-4o").unwrap().unwrap();
        assert_eq!(m.quota_limit, Some(1_000_000));
        assert_eq!(m.quota_unit.as_deref(), Some("token"));
        assert_eq!(m.quota_window.as_deref(), Some("month"));
        assert_eq!(m.used_quota, 0);
        assert_eq!(m.quota_status, "available");

        // Simulate usage exhausting the quota.
        conn.execute(
            "UPDATE discovered_models SET used_quota = 1000000, quota_status = 'exhausted'
             WHERE channel_id=1 AND raw_model_name='gpt-4o'",
            [],
        )
        .unwrap();

        // Re-discover: quota data MUST be preserved (not reset). Only the
        // discovery-related fields (is_free here) are refreshed.
        repo.upsert_discovered_model(1, "gpt-4o", true, Some("list"), None)
            .unwrap();
        let m = repo.get_discovered_model(1, "gpt-4o").unwrap().unwrap();
        assert_eq!(m.quota_limit, Some(1_000_000), "quota_limit must survive re-discovery");
        assert_eq!(m.quota_unit.as_deref(), Some("token"));
        assert_eq!(m.quota_window.as_deref(), Some("month"));
        assert_eq!(m.used_quota, 1_000_000, "used_quota must survive re-discovery");
        assert_eq!(m.quota_status, "exhausted", "quota_status must survive re-discovery");
        assert!(m.is_free, "is_free is a discovery field and should be refreshed");

        // Manual reset (total-period recovery path).
        repo.reset_discovered_model_quota(1, "gpt-4o").unwrap();
        let m = repo.get_discovered_model(1, "gpt-4o").unwrap().unwrap();
        assert_eq!(m.used_quota, 0);
        assert_eq!(m.quota_status, "available");

        // list_discovered_models returns the row.
        let all = repo.list_discovered_models().unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].raw_model_name, "gpt-4o");
    }
}
