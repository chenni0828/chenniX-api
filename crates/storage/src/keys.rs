use chennix_common::{CostTier, KeyConfig, KeyStatus, ProxyError, ProxyResult};
use rusqlite::{params, Connection, OptionalExtension};

pub struct KeyRepo<'a> {
    conn: &'a Connection,
}

impl<'a> KeyRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create_key(
        &self,
        channel_id: i64,
        api_key: &str,
        label: Option<&str>,
        cost_tier: CostTier,
        key_priority: i32,
        price_per_1k_tokens: Option<f64>,
        free_quota: Option<u64>,
        quota_reset_period: Option<&str>,
    ) -> ProxyResult<i64> {
        self.conn
            .execute(
                "INSERT INTO channel_keys
                 (channel_id, api_key, label, cost_tier, key_priority,
                  price_per_1k_tokens, free_quota, quota_reset_period)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    channel_id,
                    api_key,
                    label,
                    match cost_tier {
                        CostTier::Free => "free",
                        CostTier::Paid => "paid",
                    },
                    key_priority,
                    price_per_1k_tokens,
                    free_quota.map(|q| q as i64),
                    quota_reset_period,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_keys_for_channel(&self, channel_id: i64) -> ProxyResult<Vec<KeyConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, channel_id, api_key, label, cost_tier, key_priority,
                        price_per_1k_tokens, free_quota, used_quota, quota_reset_period, status
                 FROM channel_keys WHERE channel_id = ?1 ORDER BY key_priority, id",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![channel_id], map_key_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    pub fn get_key_by_id(&self, id: i64) -> ProxyResult<Option<KeyConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, channel_id, api_key, label, cost_tier, key_priority,
                        price_per_1k_tokens, free_quota, used_quota, quota_reset_period, status
                 FROM channel_keys WHERE id = ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<KeyConfig> = stmt
            .query_row(params![id], map_key_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn update_key_status(&self, id: i64, status: KeyStatus) -> ProxyResult<()> {
        let status_str = match status {
            KeyStatus::Active => "active",
            KeyStatus::Cooldown => "cooldown",
            KeyStatus::Disabled => "disabled",
            KeyStatus::QuotaExhausted => "quota_exhausted",
        };
        self.conn
            .execute(
                "UPDATE channel_keys SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![status_str, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn delete_key(&self, id: i64) -> ProxyResult<()> {
        self.conn
            .execute("DELETE FROM channel_keys WHERE id = ?1", params![id])
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Return the IDs of all keys whose status is `'disabled'`.
    /// Used at startup to restore disabled state from the DB into the
    /// in-memory `HealthManager`.
    pub fn get_disabled_key_ids(&self) -> ProxyResult<Vec<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id FROM channel_keys WHERE status = 'disabled'")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, i64>(0))
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut ids = Vec::new();
        for row in rows {
            ids.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(ids)
    }

    pub fn add_key_usage(&self, id: i64, tokens: u64) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE channel_keys SET used_quota = used_quota + ?1, updated_at = datetime('now') WHERE id = ?2",
                params![tokens as i64, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    // ===== Quota reset =====

    /// Reset `used_quota` to 0 for all keys with `quota_reset_period = 'daily'`.
    pub fn reset_daily_quota(&self) -> ProxyResult<usize> {
        self.conn
            .execute(
                "UPDATE channel_keys SET used_quota = 0 WHERE quota_reset_period = 'daily'",
                [],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))
    }

    /// Reset `used_quota` to 0 for all keys with `quota_reset_period = 'monthly'`.
    pub fn reset_monthly_quota(&self) -> ProxyResult<usize> {
        self.conn
            .execute(
                "UPDATE channel_keys SET used_quota = 0 WHERE quota_reset_period = 'monthly'",
                [],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))
    }

    /// Reset `used_quota` to 0 for a single key.
    pub fn reset_key_quota(&self, key_id: i64, channel_id: i64) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE channel_keys SET used_quota = 0, updated_at = datetime('now') WHERE id = ?1 AND channel_id = ?2",
                params![key_id, channel_id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    // ===== Admin API methods =====

    /// Create a key with the admin-panel's simplified parameter set.
    ///
    /// Maps the boolean `is_free` to the `cost_tier` column
    /// (`true` → `"free"`, `false` → `"paid"`).
    /// `quota_limit` maps to `free_quota` (NULL when 0).
    /// `price_per_1k_tokens` is stored as-is (NULL when 0.0).
    pub fn create_key_full(
        &self,
        channel_id: i64,
        api_key: &str,
        label: Option<&str>,
        is_free: bool,
        priority: i32,
        quota_limit: i64,
        price_per_1k_tokens: f64,
    ) -> ProxyResult<i64> {
        let cost_tier = if is_free { "free" } else { "paid" };
        let free_quota: Option<i64> = if quota_limit > 0 { Some(quota_limit) } else { None };
        let price: Option<f64> = if price_per_1k_tokens > 0.0 {
            Some(price_per_1k_tokens)
        } else {
            None
        };
        self.conn
            .execute(
                "INSERT INTO channel_keys
                 (channel_id, api_key, label, cost_tier, key_priority,
                  price_per_1k_tokens, free_quota, status)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active')",
                params![channel_id, api_key, label, cost_tier, priority, price, free_quota],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a key's configurable fields.
    ///
    /// `used_quota` is **not** touched — it is managed by the billing layer.
    /// The `status` parameter is an integer that maps to the text status:
    /// `1` → `active`, `2` → `disabled`, `3` → `cooldown`, `4` → `quota_exhausted`.
    pub fn update_key(
        &self,
        id: i64,
        api_key: &str,
        is_free: bool,
        priority: i32,
        quota_limit: i64,
        price_per_1k_tokens: f64,
        status: i32,
    ) -> ProxyResult<()> {
        let cost_tier = if is_free { "free" } else { "paid" };
        let free_quota: Option<i64> = if quota_limit > 0 { Some(quota_limit) } else { None };
        let price: Option<f64> = if price_per_1k_tokens > 0.0 {
            Some(price_per_1k_tokens)
        } else {
            None
        };
        let status_str = match status {
            1 => "active",
            2 => "disabled",
            3 => "cooldown",
            4 => "quota_exhausted",
            _ => "active",
        };
        self.conn
            .execute(
                "UPDATE channel_keys
                 SET api_key = ?1, cost_tier = ?2, key_priority = ?3,
                     price_per_1k_tokens = ?4, free_quota = ?5, status = ?6,
                     updated_at = datetime('now')
                 WHERE id = ?7",
                params![api_key, cost_tier, priority, price, free_quota, status_str, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }
}

fn map_key_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<KeyConfig> {
    let cost_str: String = r.get(4)?;
    let cost_tier = match cost_str.as_str() {
        "free" => CostTier::Free,
        _ => CostTier::Paid,
    };
    let free_quota: Option<i64> = r.get(7)?;
    let status_str: String = r.get(10)?;
    let status = match status_str.as_str() {
        "active" => KeyStatus::Active,
        "cooldown" => KeyStatus::Cooldown,
        "disabled" => KeyStatus::Disabled,
        "quota_exhausted" => KeyStatus::QuotaExhausted,
        _ => KeyStatus::Active,
    };
    Ok(KeyConfig {
        id: r.get(0)?,
        channel_id: r.get(1)?,
        api_key: r.get(2)?,
        label: r.get(3)?,
        cost_tier,
        key_priority: r.get(5)?,
        price_per_1k_tokens: r.get(6)?,
        free_quota: free_quota.map(|q| q as u64),
        used_quota: r.get::<_, i64>(8)? as u64,
        quota_reset_period: r.get(9)?,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ChannelRepo;
    use crate::schema::init_db;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let ch_repo = ChannelRepo::new(&conn);
        ch_repo
            .create_channel("test", &chennix_common::ChannelProvider::OpenaiCompatible, "http://test")
            .unwrap();
        conn
    }

    #[test]
    fn test_key_crud() {
        let conn = setup();
        let repo = KeyRepo::new(&conn);
        let id = repo
            .create_key(1, "sk-test", Some("label1"), CostTier::Free, 1, None, Some(10000), Some("monthly"))
            .unwrap();
        let keys = repo.get_keys_for_channel(1).unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].api_key, "sk-test");
        assert_eq!(keys[0].cost_tier, CostTier::Free);
        // Default status on creation should be 'active' (DB default).
        assert_eq!(keys[0].status, KeyStatus::Active);

        // Round-trip: update status to Disabled and read back.
        repo.update_key_status(id, KeyStatus::Disabled).unwrap();
        repo.add_key_usage(id, 500).unwrap();
        let key = repo.get_key_by_id(id).unwrap().unwrap();
        assert_eq!(key.used_quota, 500);
        assert_eq!(key.status, KeyStatus::Disabled);

        repo.delete_key(id).unwrap();
        assert!(repo.get_key_by_id(id).unwrap().is_none());
    }

    #[test]
    fn test_key_status_round_trip_all_variants() {
        let conn = setup();
        let repo = KeyRepo::new(&conn);
        let id = repo
            .create_key(1, "sk-rt", None, CostTier::Paid, 1, None, None, None)
            .unwrap();
        for status in [
            KeyStatus::Active,
            KeyStatus::Cooldown,
            KeyStatus::Disabled,
            KeyStatus::QuotaExhausted,
        ] {
            repo.update_key_status(id, status).unwrap();
            let key = repo.get_key_by_id(id).unwrap().unwrap();
            assert_eq!(key.status, status, "status round-trip failed for {:?}", status);
        }
    }

    #[test]
    fn test_get_key_by_id_missing_returns_none() {
        let conn = setup();
        let repo = KeyRepo::new(&conn);
        // No row → None, not an error.
        assert!(repo.get_key_by_id(99999).unwrap().is_none());
    }

    #[test]
    fn test_real_db_error_surfaces_as_storage_error() {
        let conn = setup();
        let repo = KeyRepo::new(&conn);
        // Drop the table to force a real DB error (not QueryReturnedNoRows).
        conn.execute("DROP TABLE channel_keys", []).unwrap();
        let result = repo.get_key_by_id(1);
        assert!(result.is_err(), "expected Err, got {:?}", result);
        match result.unwrap_err() {
            ProxyError::Storage(_) => {} // good
            other => panic!("expected ProxyError::Storage, got {:?}", other),
        }
    }

    #[test]
    fn test_reset_daily_quota() {
        let conn = setup();
        let repo = KeyRepo::new(&conn);
        // Create two daily keys and one monthly key.
        let id1 = repo.create_key(1, "sk-d1", None, CostTier::Paid, 1, None, None, Some("daily")).unwrap();
        let id2 = repo.create_key(1, "sk-d2", None, CostTier::Paid, 1, None, None, Some("daily")).unwrap();
        let id3 = repo.create_key(1, "sk-m1", None, CostTier::Paid, 1, None, None, Some("monthly")).unwrap();
        // Add usage to all.
        repo.add_key_usage(id1, 100).unwrap();
        repo.add_key_usage(id2, 200).unwrap();
        repo.add_key_usage(id3, 300).unwrap();
        // Reset daily.
        let count = repo.reset_daily_quota().unwrap();
        assert_eq!(count, 2);
        // Daily keys should be zeroed.
        assert_eq!(repo.get_key_by_id(id1).unwrap().unwrap().used_quota, 0);
        assert_eq!(repo.get_key_by_id(id2).unwrap().unwrap().used_quota, 0);
        // Monthly key should be untouched.
        assert_eq!(repo.get_key_by_id(id3).unwrap().unwrap().used_quota, 300);
    }

    #[test]
    fn test_reset_monthly_quota() {
        let conn = setup();
        let repo = KeyRepo::new(&conn);
        let id_daily = repo.create_key(1, "sk-d1", None, CostTier::Paid, 1, None, None, Some("daily")).unwrap();
        let id_monthly = repo.create_key(1, "sk-m1", None, CostTier::Paid, 1, None, None, Some("monthly")).unwrap();
        repo.add_key_usage(id_daily, 100).unwrap();
        repo.add_key_usage(id_monthly, 500).unwrap();
        let count = repo.reset_monthly_quota().unwrap();
        assert_eq!(count, 1);
        assert_eq!(repo.get_key_by_id(id_daily).unwrap().unwrap().used_quota, 100);
        assert_eq!(repo.get_key_by_id(id_monthly).unwrap().unwrap().used_quota, 0);
    }

    #[test]
    fn test_reset_key_quota_single() {
        let conn = setup();
        let repo = KeyRepo::new(&conn);
        let id1 = repo.create_key(1, "sk-a", None, CostTier::Paid, 1, None, None, Some("daily")).unwrap();
        let id2 = repo.create_key(1, "sk-b", None, CostTier::Paid, 1, None, None, Some("daily")).unwrap();
        repo.add_key_usage(id1, 100).unwrap();
        repo.add_key_usage(id2, 200).unwrap();
        repo.reset_key_quota(id1, 1).unwrap();
        assert_eq!(repo.get_key_by_id(id1).unwrap().unwrap().used_quota, 0);
        assert_eq!(repo.get_key_by_id(id2).unwrap().unwrap().used_quota, 200);
    }
}
