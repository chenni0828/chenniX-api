use chennix_common::{BillingType, ChannelModelPricing, ModelBinding, ProxyError, ProxyResult};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub struct ModelRepo<'a> {
    conn: &'a Connection,
}

impl<'a> ModelRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create_model(&self, canonical_name: &str) -> ProxyResult<i64> {
        self.conn
            .execute(
                "INSERT INTO models (canonical_name) VALUES (?1)",
                params![canonical_name],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn rename_model(&self, model_id: i64, new_name: &str) -> ProxyResult<()> {
        let affected = self
            .conn
            .execute(
                "UPDATE models SET canonical_name = ?1 WHERE id = ?2",
                params![new_name, model_id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        if affected == 0 {
            return Err(ProxyError::ModelNotFound(model_id.to_string()));
        }
        Ok(())
    }

    pub fn delete_model(&self, model_id: i64) -> ProxyResult<()> {
        self.conn
            .execute("DELETE FROM models WHERE id = ?1", params![model_id])
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_model_by_name(&self, name: &str) -> ProxyResult<Option<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, canonical_name FROM models WHERE canonical_name = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<(i64, String)> = stmt
            .query_row(params![name], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn get_model_by_id(&self, id: i64) -> ProxyResult<Option<(i64, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, canonical_name FROM models WHERE id = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<(i64, String)> = stmt
            .query_row(params![id], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn add_binding(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream_model_name: &str,
        priority: i32,
    ) -> ProxyResult<()> {
        self.add_binding_with_weight(model_id, channel_id, upstream_model_name, priority, 1)
    }

    /// 与 `add_binding` 相同，但允许指定 `weight`（load_balance 策略使用）。
    /// `weight` 必须 >= 1，调用方负责校验。
    pub fn add_binding_with_weight(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream_model_name: &str,
        priority: i32,
        weight: i32,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "INSERT INTO model_channels
                    (model_id, channel_id, upstream_model_name, priority, weight)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(model_id, channel_id, upstream_model_name) DO UPDATE SET
                    upstream_model_name = excluded.upstream_model_name,
                    priority = excluded.priority,
                    weight = excluded.weight",
                params![model_id, channel_id, upstream_model_name, priority, weight],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 读取某个大模型的路由策略（`'priority'` 或 `'load_balance'`）。
    /// 模型不存在时返回 `None`。
    pub fn get_routing_strategy(&self, model_id: i64) -> ProxyResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT routing_strategy FROM models WHERE id = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<String> = stmt
            .query_row(params![model_id], |r| r.get(0))
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    /// 更新某个大模型的路由策略。模型不存在时返回错误。
    pub fn update_routing_strategy(&self, model_id: i64, strategy: &str) -> ProxyResult<()> {
        let affected = self
            .conn
            .execute(
                "UPDATE models SET routing_strategy = ?1 WHERE id = ?2",
                params![strategy, model_id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        if affected == 0 {
            return Err(ProxyError::ModelNotFound(model_id.to_string()));
        }
        Ok(())
    }

    /// 删除某个 `(model_id, channel_id, upstream_model_name)` 三元组绑定。
    pub fn remove_binding(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream_model_name: &str,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "DELETE FROM model_channels
                 WHERE model_id = ?1 AND channel_id = ?2 AND upstream_model_name = ?3",
                params![model_id, channel_id, upstream_model_name],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 删除某大模型在某渠道下的全部绑定（忽略 upstream_model_name）。
    /// 用于"从渠道移除该模型"的语义（一个模型在同一渠道可能有多 upstream 绑定）。
    pub fn remove_all_bindings_for_channel(
        &self,
        model_id: i64,
        channel_id: i64,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "DELETE FROM model_channels WHERE model_id = ?1 AND channel_id = ?2",
                params![model_id, channel_id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 重新排序某模型下绑定的 priority。
    /// `ordered_bindings` 是 `(channel_id, upstream_model_name)` 列表，按调用
    /// 优先级从高到低排列（索引 0 = 最高优先级）。priority 按位置赋值为
    /// `(i+1)*10`，全部在一个事务里提交。
    pub fn reorder_bindings(
        &self,
        model_id: i64,
        ordered_bindings: &[(i64, String)],
    ) -> ProxyResult<()> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        for (i, (cid, upstream)) in ordered_bindings.iter().enumerate() {
            tx.execute(
                "UPDATE model_channels SET priority = ?1
                 WHERE model_id = ?2 AND channel_id = ?3 AND upstream_model_name = ?4",
                params![(i as i32 + 1) * 10, model_id, cid, upstream],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        }
        tx.commit()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// 更新某个三元组绑定的 `weight`（load_balance 策略使用）。
    /// 调用方负责校验 `weight >= 1`。绑定不存在时返回错误。
    pub fn update_binding_weight(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream_model_name: &str,
        weight: i32,
    ) -> ProxyResult<()> {
        let affected = self
            .conn
            .execute(
                "UPDATE model_channels SET weight = ?1
                 WHERE model_id = ?2 AND channel_id = ?3 AND upstream_model_name = ?4",
                params![weight, model_id, channel_id, upstream_model_name],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        if affected == 0 {
            return Err(ProxyError::Storage(format!(
                "binding (model={}, channel={}, upstream={}) not found",
                model_id, channel_id, upstream_model_name
            )));
        }
        Ok(())
    }

    pub fn get_bindings_for_model(&self, model_id: i64) -> ProxyResult<Vec<ModelBinding>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT mc.model_id, m.canonical_name, mc.channel_id, mc.upstream_model_name, mc.priority, mc.weight
                 FROM model_channels mc
                 JOIN models m ON mc.model_id = m.id
                 WHERE mc.model_id = ?1
                 ORDER BY mc.priority ASC, mc.channel_id ASC",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![model_id], |r| {
                Ok(ModelBinding {
                    model_id: r.get(0)?,
                    canonical_name: r.get(1)?,
                    channel_id: r.get(2)?,
                    upstream_model_name: r.get(3)?,
                    priority: r.get(4)?,
                    weight: r.get(5).unwrap_or(1),
                })
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    pub fn get_bindings_for_channel(&self, channel_id: i64) -> ProxyResult<Vec<ModelBinding>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT mc.model_id, m.canonical_name, mc.channel_id, mc.upstream_model_name, mc.priority, mc.weight
                 FROM model_channels mc
                 JOIN models m ON mc.model_id = m.id
                 WHERE mc.channel_id = ?1
                 ORDER BY mc.priority ASC, mc.model_id ASC",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![channel_id], |r| {
                Ok(ModelBinding {
                    model_id: r.get(0)?,
                    canonical_name: r.get(1)?,
                    channel_id: r.get(2)?,
                    upstream_model_name: r.get(3)?,
                    priority: r.get(4)?,
                    weight: r.get(5).unwrap_or(1),
                })
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// 列出全部大模型，返回 `(id, canonical_name, routing_strategy)`。
    /// `routing_strategy` 为 `'priority'` 或 `'load_balance'`。
    pub fn list_all_models(&self) -> ProxyResult<Vec<(i64, String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, canonical_name, routing_strategy FROM models ORDER BY id")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// 获取某个 `(model_id, channel_id, upstream_model_name)` 绑定的定价配置。
    pub fn get_binding_pricing(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream_model_name: &str,
    ) -> ProxyResult<Option<ChannelModelPricing>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT billing_type, input_price, output_price, call_price, billing_expr
                 FROM model_channels
                 WHERE model_id = ?1 AND channel_id = ?2 AND upstream_model_name = ?3",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<ChannelModelPricing> = stmt
            .query_row(params![model_id, channel_id, upstream_model_name], |r| {
                let billing_type = r.get::<_, i32>(0).unwrap_or(0);
                let billing_expr: Option<String> = r.get(4).ok();
                Ok(ChannelModelPricing {
                    billing_type: BillingType::from_i32(billing_type),
                    input_price: r.get(1).unwrap_or(0.0),
                    output_price: r.get(2).unwrap_or(0.0),
                    call_price: r.get(3).unwrap_or(0.0),
                    billing_expr,
                })
            })
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    /// 更新某个 `(model_id, channel_id, upstream_model_name)` 绑定的定价配置。
    /// 如果该绑定不存在则返回错误。
    pub fn update_binding_pricing(
        &self,
        model_id: i64,
        channel_id: i64,
        upstream_model_name: &str,
        pricing: &ChannelModelPricing,
    ) -> ProxyResult<()> {
        let affected = self
            .conn
            .execute(
                "UPDATE model_channels
                 SET billing_type = ?1, input_price = ?2, output_price = ?3,
                     call_price = ?4, billing_expr = ?5
                 WHERE model_id = ?6 AND channel_id = ?7 AND upstream_model_name = ?8",
                params![
                    pricing.billing_type.as_i32(),
                    pricing.input_price,
                    pricing.output_price,
                    pricing.call_price,
                    pricing.billing_expr,
                    model_id,
                    channel_id,
                    upstream_model_name,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        if affected == 0 {
            return Err(ProxyError::Storage(format!(
                "binding (model={}, channel={}, upstream={}) not found",
                model_id, channel_id, upstream_model_name
            )));
        }
        Ok(())
    }

    /// 返回所有 model_channels 绑定及其定价（用于定价总览页面）。
    /// 每行包含 model_id, canonical_name, channel_id, channel_name,
    /// upstream_model_name, priority, weight, pricing。
    pub fn list_all_bindings_with_pricing(
        &self,
    ) -> ProxyResult<Vec<BindingPricingRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT mc.model_id, m.canonical_name,
                        mc.channel_id, c.name,
                        mc.upstream_model_name,
                        mc.billing_type, mc.input_price, mc.output_price,
                        mc.call_price, mc.billing_expr,
                        mc.priority, mc.weight
                 FROM model_channels mc
                 JOIN models m ON mc.model_id = m.id
                 JOIN channels c ON mc.channel_id = c.id
                 ORDER BY m.canonical_name, c.name",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |r| {
                let billing_type = r.get::<_, i32>(5).unwrap_or(0);
                let billing_expr: Option<String> = r.get(9).ok();
                Ok(BindingPricingRow {
                    model_id: r.get(0)?,
                    canonical_name: r.get(1)?,
                    channel_id: r.get(2)?,
                    channel_name: r.get(3)?,
                    upstream_model_name: r.get(4)?,
                    priority: r.get(10).unwrap_or(100),
                    weight: r.get(11).unwrap_or(1),
                    pricing: ChannelModelPricing {
                        billing_type: BillingType::from_i32(billing_type),
                        input_price: r.get(6).unwrap_or(0.0),
                        output_price: r.get(7).unwrap_or(0.0),
                        call_price: r.get(8).unwrap_or(0.0),
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

/// 一行渠道-模型绑定及其定价（用于定价总览页面）。
#[derive(Debug, Clone, Serialize)]
pub struct BindingPricingRow {
    pub model_id: i64,
    pub canonical_name: String,
    pub channel_id: i64,
    pub channel_name: String,
    pub upstream_model_name: Option<String>,
    /// 该绑定的调用优先级（数字越小越优先）。
    pub priority: i32,
    /// 该绑定的负载均衡权重（>=1，仅 load_balance 策略生效）。
    pub weight: i32,
    pub pricing: ChannelModelPricing,
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
    fn test_create_and_get_model() {
        let conn = setup();
        let repo = ModelRepo::new(&conn);
        let id = repo.create_model("deepseek-v3").unwrap();
        let model = repo.get_model_by_name("deepseek-v3").unwrap();
        assert_eq!(model.unwrap().1, "deepseek-v3");
        let by_id = repo.get_model_by_id(id).unwrap();
        assert_eq!(by_id.unwrap().0, id);
    }

    #[test]
    fn test_get_model_missing_returns_none() {
        let conn = setup();
        let repo = ModelRepo::new(&conn);
        assert!(repo.get_model_by_name("nope").unwrap().is_none());
        assert!(repo.get_model_by_id(99999).unwrap().is_none());
    }

    #[test]
    fn test_real_db_error_surfaces_as_storage_error() {
        let conn = setup();
        let repo = ModelRepo::new(&conn);
        conn.execute("DROP TABLE models", []).unwrap();
        let result = repo.get_model_by_name("x");
        assert!(result.is_err(), "expected Err, got {:?}", result);
        match result.unwrap_err() {
            ProxyError::Storage(_) => {}
            other => panic!("expected ProxyError::Storage, got {:?}", other),
        }
    }

    #[test]
    fn test_rename_model() {
        let conn = setup();
        let repo = ModelRepo::new(&conn);
        let id = repo.create_model("old-name").unwrap();
        repo.rename_model(id, "new-name").unwrap();
        assert!(repo.get_model_by_name("old-name").unwrap().is_none());
        assert_eq!(repo.get_model_by_name("new-name").unwrap().unwrap().0, id);
    }

    #[test]
    fn test_binding_crud() {
        let conn = setup();
        // 需要先建渠道
        conn.execute(
            "INSERT INTO channels (id, name, provider, base_url) VALUES (1, 'test', 'openai-compatible', 'http://test')",
            [],
        ).unwrap();
        let repo = ModelRepo::new(&conn);
        let model_id = repo.create_model("test-model").unwrap();
        repo.add_binding(model_id, 1, "upstream-name", 100).unwrap();
        let bindings = repo.get_bindings_for_model(model_id).unwrap();
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].upstream_model_name, "upstream-name");
        assert_eq!(bindings[0].priority, 100);
        repo.remove_binding(model_id, 1, "upstream-name").unwrap();
        let bindings = repo.get_bindings_for_model(model_id).unwrap();
        assert_eq!(bindings.len(), 0);
    }

    #[test]
    fn test_reorder_bindings() {
        let conn = setup();
        conn.execute(
            "INSERT INTO channels (id, name, provider, base_url) VALUES (1, 'ch-a', 'openai-compatible', 'http://a')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO channels (id, name, provider, base_url) VALUES (2, 'ch-b', 'openai-compatible', 'http://b')",
            [],
        ).unwrap();
        let repo = ModelRepo::new(&conn);
        let model_id = repo.create_model("m").unwrap();
        // 按默认 priority 100 创建两个绑定
        repo.add_binding(model_id, 1, "u-a", 100).unwrap();
        repo.add_binding(model_id, 2, "u-b", 100).unwrap();

        // 让 channel 2 优先于 channel 1
        repo.reorder_bindings(model_id, &[(2, "u-b".to_string()), (1, "u-a".to_string())])
            .unwrap();

        let bindings = repo.get_bindings_for_model(model_id).unwrap();
        assert_eq!(bindings.len(), 2);
        // 排序后 channel 2 在前
        assert_eq!(bindings[0].channel_id, 2);
        assert_eq!(bindings[0].priority, 10);
        assert_eq!(bindings[1].channel_id, 1);
        assert_eq!(bindings[1].priority, 20);
    }
}
