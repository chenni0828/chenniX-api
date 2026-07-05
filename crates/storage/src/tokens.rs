use chennix_common::{AuthContext, ProxyError, ProxyResult, TokenConfig};
use rusqlite::{params, Connection, OptionalExtension};

use crate::now_iso8601;
use crate::users::UserRepo;

pub struct TokenRepo<'a> {
    conn: &'a Connection,
}

impl<'a> TokenRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn create_token(
        &self,
        user_id: i64,
        key: &str,
        name: Option<&str>,
        remain_quota: i64,
        unlimited_quota: bool,
    ) -> ProxyResult<i64> {
        let now = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO tokens (user_id, key, name, remain_quota, used_quota,
                                     unlimited_quota, expired_time, model_limits_enabled,
                                     model_limits, status, allow_ips,
                                     created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, -1, 0, NULL, 1, NULL, ?6, ?6)",
                params![
                    user_id,
                    key,
                    name,
                    remain_quota,
                    if unlimited_quota { 1 } else { 0 },
                    now,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_token_by_key(&self, key: &str) -> ProxyResult<Option<TokenConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, user_id, key, name, remain_quota, used_quota, unlimited_quota,
                        expired_time, model_limits_enabled, model_limits, status, allow_ips
                 FROM tokens WHERE key = ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<TokenConfig> = stmt
            .query_row(params![key], map_token_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn get_token_by_id(&self, id: i64) -> ProxyResult<Option<TokenConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, user_id, key, name, remain_quota, used_quota, unlimited_quota,
                        expired_time, model_limits_enabled, model_limits, status, allow_ips
                 FROM tokens WHERE id = ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<TokenConfig> = stmt
            .query_row(params![id], map_token_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn get_tokens_for_user(&self, user_id: i64) -> ProxyResult<Vec<TokenConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, user_id, key, name, remain_quota, used_quota, unlimited_quota,
                        expired_time, model_limits_enabled, model_limits, status, allow_ips
                 FROM tokens WHERE user_id = ?1 ORDER BY id",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![user_id], map_token_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// Atomically increment `remain_quota` by `delta` (use negative delta to decrement).
    pub fn update_remain_quota_delta(&self, id: i64, delta: i64) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE tokens SET remain_quota = remain_quota + ?1, updated_at = ?2
                 WHERE id = ?3",
                params![delta, now_iso8601(), id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Atomically increment `used_quota` by `delta` (use negative delta to decrement).
    pub fn update_used_quota_delta(&self, id: i64, delta: i64) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE tokens SET used_quota = used_quota + ?1, updated_at = ?2
                 WHERE id = ?3",
                params![delta, now_iso8601(), id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn update_status(&self, id: i64, status: i32) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE tokens SET status = ?1, updated_at = ?2 WHERE id = ?3",
                params![status, now_iso8601(), id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Delete a token. Both `id` AND `user_id` must match to prevent cross-user deletion.
    /// Returns Ok(()) even if no row was deleted (idempotent); callers that need to
    /// distinguish "not found / not owned" from "deleted" should check the affected row
    /// count via a separate lookup.
    pub fn delete_token(&self, id: i64, user_id: i64) -> ProxyResult<()> {
        self.conn
            .execute(
                "DELETE FROM tokens WHERE id = ?1 AND user_id = ?2",
                params![id, user_id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn set_model_limits(&self, id: i64, enabled: bool, models: &[String]) -> ProxyResult<()> {
        let json = if models.is_empty() {
            "[]".to_string()
        } else {
            serde_json::to_string(models).map_err(|e| ProxyError::Storage(e.to_string()))?
        };
        self.conn
            .execute(
                "UPDATE tokens SET model_limits_enabled = ?1, model_limits = ?2,
                                   updated_at = ?3
                 WHERE id = ?4",
                params![if enabled { 1 } else { 0 }, json, now_iso8601(), id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_remain_quota(&self, id: i64) -> ProxyResult<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT remain_quota FROM tokens WHERE id = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<i64> = stmt
            .query_row(params![id], |r| r.get(0))
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    // ===== Admin API methods =====

    /// List all tokens, or filter by `user_id` when `Some`.
    ///
    /// When `user_id` is `None`, returns every token in the database (admin view).
    /// When `user_id` is `Some(uid)`, returns only that user's tokens.
    pub fn list_tokens(&self, user_id: Option<i64>) -> ProxyResult<Vec<TokenConfig>> {
        let mut sql = String::from(
            "SELECT id, user_id, key, name, remain_quota, used_quota, unlimited_quota,
                    expired_time, model_limits_enabled, model_limits, status, allow_ips
             FROM tokens",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        if let Some(uid) = user_id {
            sql.push_str(" WHERE user_id = ?1");
            params_vec.push(Box::new(uid));
        }
        sql.push_str(" ORDER BY id");
        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec.iter()), map_token_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// Create a token with full control over all configurable fields.
    ///
    /// This is the admin-panel variant of `create_token` — it accepts every
    /// field that the admin UI exposes (model limits, IP whitelist, expiry,
    /// etc.).
    ///
    /// # Parameters
    /// - `model_limits` — JSON array string, e.g. `["gpt-4","claude-3"]`.
    ///   Pass an empty string to store NULL.
    /// - `allow_ips` — comma-separated IP string, e.g. `127.0.0.1,10.0.0.5`.
    ///   Pass an empty string to store NULL.
    pub fn create_token_full(
        &self,
        user_id: i64,
        name: &str,
        key: &str,
        remain_quota: i64,
        unlimited_quota: bool,
        expired_time: i64,
        model_limits: &str,
        model_limits_enabled: bool,
        allow_ips: &str,
    ) -> ProxyResult<i64> {
        let model_limits_val: Option<&str> = if model_limits.is_empty() {
            None
        } else {
            Some(model_limits)
        };
        let allow_ips_val: Option<&str> = if allow_ips.is_empty() {
            None
        } else {
            Some(allow_ips)
        };
        let name_val: Option<&str> = if name.is_empty() { None } else { Some(name) };
        let now = now_iso8601();
        self.conn
            .execute(
                "INSERT INTO tokens
                 (user_id, key, name, remain_quota, used_quota, unlimited_quota,
                  expired_time, model_limits_enabled, model_limits, status, allow_ips,
                  created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7, ?8, 1, ?9, ?10, ?10)",
                params![
                    user_id,
                    key,
                    name_val,
                    remain_quota,
                    if unlimited_quota { 1 } else { 0 },
                    expired_time,
                    if model_limits_enabled { 1 } else { 0 },
                    model_limits_val,
                    allow_ips_val,
                    now,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a token's configurable fields.
    ///
    /// `used_quota` is **not** touched — it is managed by the billing layer.
    /// The `key` field is **not** changed — it is the primary identifier and
    /// should not be mutated after creation.
    pub fn update_token(
        &self,
        id: i64,
        name: &str,
        remain_quota: i64,
        unlimited_quota: bool,
        expired_time: i64,
        model_limits: &str,
        model_limits_enabled: bool,
        allow_ips: &str,
        status: i32,
    ) -> ProxyResult<()> {
        let model_limits_val: Option<&str> = if model_limits.is_empty() {
            None
        } else {
            Some(model_limits)
        };
        let allow_ips_val: Option<&str> = if allow_ips.is_empty() {
            None
        } else {
            Some(allow_ips)
        };
        let name_val: Option<&str> = if name.is_empty() { None } else { Some(name) };
        self.conn
            .execute(
                "UPDATE tokens
                 SET name = ?1, remain_quota = ?2, unlimited_quota = ?3,
                     expired_time = ?4, model_limits_enabled = ?5, model_limits = ?6,
                     allow_ips = ?7, status = ?8, updated_at = ?9
                 WHERE id = ?10",
                params![
                    name_val,
                    remain_quota,
                    if unlimited_quota { 1 } else { 0 },
                    expired_time,
                    if model_limits_enabled { 1 } else { 0 },
                    model_limits_val,
                    allow_ips_val,
                    status,
                    now_iso8601(),
                    id,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Delete a token by ID (admin variant — no user_id ownership check).
    ///
    /// Unlike `delete_token(id, user_id)` which requires both id and user_id
    /// to match, this method deletes unconditionally by id. Use it only in
    /// admin contexts where the caller has already been authorized.
    pub fn delete_token_by_id(&self, id: i64) -> ProxyResult<()> {
        self.conn
            .execute("DELETE FROM tokens WHERE id = ?1", params![id])
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Full validation: key → token → user.
    ///
    /// Checks (in order): token exists, token.status == 1, token not expired,
    /// IP whitelist (if set), user exists, user.status == 1.
    ///
    /// Returns `Some(AuthContext)` if valid, `None` if any check fails.
    /// Storage errors propagate as `Err`.
    pub fn validate_token(
        &self,
        key: &str,
        client_ip: Option<&str>,
    ) -> ProxyResult<Option<AuthContext>> {
        let token = match self.get_token_by_key(key)? {
            Some(t) => t,
            None => return Ok(None),
        };

        // 1. token status must be enabled (1)
        if token.status != 1 {
            return Ok(None);
        }

        // 2. expiry check (-1 means never expire)
        let now = chrono::Utc::now().timestamp();
        if token.expired_time != -1 && token.expired_time <= now {
            return Ok(None);
        }

        // 3. IP whitelist (if allow_ips is set, client_ip must be present and in the list)
        if let Some(ips) = &token.allow_ips {
            match client_ip {
                Some(ip) => {
                    if !ips.iter().any(|allowed| allowed == ip) {
                        return Ok(None);
                    }
                }
                None => return Ok(None),
            }
        }

        // 4. fetch the user
        let user_repo = UserRepo::new(self.conn);
        let user = match user_repo.get_user_by_id(token.user_id)? {
            Some(u) => u,
            None => return Ok(None),
        };

        // 5. user status must be enabled (1)
        if user.status != 1 {
            return Ok(None);
        }

        Ok(Some(AuthContext { user, token, client_ip: client_ip.map(|s| s.to_string()) }))
    }
}

fn map_token_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<TokenConfig> {
    let unlimited: i64 = r.get(6)?;
    let limits_enabled: i64 = r.get(8)?;
    let model_limits_str: Option<String> = r.get(9)?;
    let allow_ips_str: Option<String> = r.get(11)?;

    let model_limits = model_limits_str.map(|s| {
        if s.is_empty() {
            Vec::new()
        } else {
            serde_json::from_str::<Vec<String>>(&s).unwrap_or_default()
        }
    });

    let allow_ips = allow_ips_str.map(|s| {
        s.split(',')
            .map(|ip| ip.trim().to_string())
            .filter(|ip| !ip.is_empty())
            .collect::<Vec<String>>()
    });

    Ok(TokenConfig {
        id: r.get(0)?,
        user_id: r.get(1)?,
        key: r.get(2)?,
        name: r.get(3)?,
        remain_quota: r.get(4)?,
        used_quota: r.get(5)?,
        unlimited_quota: unlimited != 0,
        expired_time: r.get(7)?,
        model_limits_enabled: limits_enabled != 0,
        model_limits,
        status: r.get(10)?,
        allow_ips,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::init_db;
    use crate::users::UserRepo;

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let user_repo = UserRepo::new(&conn);
        user_repo.create_user("alice", "hash", 1, "default").unwrap();
        conn
    }

    #[test]
    fn test_create_and_get_token() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo
            .create_token(1, "sk-abc", Some("my token"), 1000, false)
            .unwrap();
        assert!(id > 0);

        let by_key = repo.get_token_by_key("sk-abc").unwrap().unwrap();
        assert_eq!(by_key.id, id);
        assert_eq!(by_key.user_id, 1);
        assert_eq!(by_key.key, "sk-abc");
        assert_eq!(by_key.name.as_deref(), Some("my token"));
        assert_eq!(by_key.remain_quota, 1000);
        assert_eq!(by_key.used_quota, 0);
        assert!(!by_key.unlimited_quota);
        assert_eq!(by_key.expired_time, -1);
        assert!(!by_key.model_limits_enabled);
        assert_eq!(by_key.model_limits, None);
        assert_eq!(by_key.status, 1);
        assert_eq!(by_key.allow_ips, None);

        let by_id = repo.get_token_by_id(id).unwrap().unwrap();
        assert_eq!(by_id.key, "sk-abc");

        let tokens = repo.get_tokens_for_user(1).unwrap();
        assert_eq!(tokens.len(), 1);
    }

    #[test]
    fn test_get_token_missing_returns_none() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        assert!(repo.get_token_by_key("nope").unwrap().is_none());
        assert!(repo.get_token_by_id(99999).unwrap().is_none());
        assert!(repo.get_remain_quota(99999).unwrap().is_none());
        assert!(repo.get_tokens_for_user(99999).unwrap().is_empty());
    }

    #[test]
    fn test_update_remain_quota_delta_atomic() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-q", None, 500, false).unwrap();

        repo.update_remain_quota_delta(id, -100).unwrap();
        repo.update_remain_quota_delta(id, -50).unwrap();
        assert_eq!(repo.get_remain_quota(id).unwrap(), Some(350));

        // top-up
        repo.update_remain_quota_delta(id, 200).unwrap();
        assert_eq!(repo.get_remain_quota(id).unwrap(), Some(550));
    }

    #[test]
    fn test_update_used_quota_delta_atomic() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-u", None, 500, false).unwrap();

        repo.update_used_quota_delta(id, 30).unwrap();
        repo.update_used_quota_delta(id, 70).unwrap();
        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert_eq!(t.used_quota, 100);
    }

    #[test]
    fn test_update_status() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-s", None, 100, false).unwrap();

        repo.update_status(id, 2).unwrap();
        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert_eq!(t.status, 2);
        assert!(!t.is_enabled());

        repo.update_status(id, 3).unwrap();
        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert_eq!(t.status, 3);

        repo.update_status(id, 1).unwrap();
        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert!(t.is_enabled());
    }

    #[test]
    fn test_delete_token_user_id_check_prevents_cross_user_deletion() {
        let conn = setup();
        let user_repo = UserRepo::new(&conn);
        user_repo.create_user("bob", "hash", 1, "default").unwrap();
        // bob has id=2 (alice is id=1)

        let repo = TokenRepo::new(&conn);
        // alice's token
        let alice_tok = repo.create_token(1, "sk-alice", None, 100, false).unwrap();
        // bob's token
        let bob_tok = repo.create_token(2, "sk-bob", None, 100, false).unwrap();

        // bob tries to delete alice's token by id but passes his own user_id → no-op
        repo.delete_token(alice_tok, 2).unwrap();
        assert!(repo.get_token_by_id(alice_tok).unwrap().is_some(),
            "cross-user delete must not remove alice's token");

        // bob deletes his own token → succeeds
        repo.delete_token(bob_tok, 2).unwrap();
        assert!(repo.get_token_by_id(bob_tok).unwrap().is_none());

        // alice deletes her own token → succeeds
        repo.delete_token(alice_tok, 1).unwrap();
        assert!(repo.get_token_by_id(alice_tok).unwrap().is_none());
    }

    #[test]
    fn test_set_model_limits_and_parsing() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-m", None, 100, false).unwrap();

        let models = vec!["gpt-4".to_string(), "claude-3".to_string()];
        repo.set_model_limits(id, true, &models).unwrap();

        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert!(t.model_limits_enabled);
        let limits = t.model_limits.clone().unwrap();
        assert_eq!(limits, vec!["gpt-4".to_string(), "claude-3".to_string()]);

        // allows_model checks
        assert!(t.allows_model("gpt-4"));
        assert!(t.allows_model("claude-3"));
        assert!(!t.allows_model("gemini"));

        // disable limits → all models allowed
        repo.set_model_limits(id, false, &models).unwrap();
        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert!(!t.model_limits_enabled);
        assert!(t.allows_model("anything"));

        // empty list when enabled → nothing allowed
        repo.set_model_limits(id, true, &[]).unwrap();
        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert!(t.model_limits_enabled);
        assert_eq!(t.model_limits, Some(Vec::new()));
        assert!(!t.allows_model("gpt-4"));
    }

    #[test]
    fn test_allow_ips_parsing() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-ip", None, 100, false).unwrap();

        // Insert allow_ips directly as comma-separated string
        conn.execute(
            "UPDATE tokens SET allow_ips = '127.0.0.1, 10.0.0.5 ,192.168.1.1' WHERE id = ?1",
            params![id],
        )
        .unwrap();

        let t = repo.get_token_by_id(id).unwrap().unwrap();
        let ips = t.allow_ips.clone().unwrap();
        assert_eq!(ips.len(), 3);
        assert_eq!(ips[0], "127.0.0.1");
        assert_eq!(ips[1], "10.0.0.5"); // trimmed
        assert_eq!(ips[2], "192.168.1.1");

        assert!(t.allows_ip("127.0.0.1"));
        assert!(t.allows_ip("10.0.0.5"));
        assert!(!t.allows_ip("8.8.8.8"));
    }

    // ===== validate_token tests =====

    #[test]
    fn test_validate_token_valid() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        repo.create_token(1, "sk-valid", None, 1000, false).unwrap();

        let ctx = repo.validate_token("sk-valid", None).unwrap();
        assert!(ctx.is_some(), "valid token should produce AuthContext");
        let ctx = ctx.unwrap();
        assert_eq!(ctx.user.username, "alice");
        assert_eq!(ctx.token.key, "sk-valid");
        assert_eq!(ctx.user.id, ctx.token.user_id);
    }

    #[test]
    fn test_validate_token_unknown_key() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let ctx = repo.validate_token("sk-nonexistent", None).unwrap();
        assert!(ctx.is_none());
    }

    #[test]
    fn test_validate_token_disabled_token() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-disabled", None, 1000, false).unwrap();
        repo.update_status(id, 2).unwrap();

        let ctx = repo.validate_token("sk-disabled", None).unwrap();
        assert!(ctx.is_none(), "disabled token must not validate");
    }

    #[test]
    fn test_validate_token_expired() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-expired", None, 1000, false).unwrap();

        // set expired_time to past
        let past = chrono::Utc::now().timestamp() - 3600;
        conn.execute(
            "UPDATE tokens SET expired_time = ?1 WHERE id = ?2",
            params![past, id],
        )
        .unwrap();

        let ctx = repo.validate_token("sk-expired", None).unwrap();
        assert!(ctx.is_none(), "expired token must not validate");

        // future expiry should be valid
        let future = chrono::Utc::now().timestamp() + 3600;
        conn.execute(
            "UPDATE tokens SET expired_time = ?1 WHERE id = ?2",
            params![future, id],
        )
        .unwrap();
        let ctx = repo.validate_token("sk-expired", None).unwrap();
        assert!(ctx.is_some(), "token with future expiry should validate");

        // -1 means never expire
        conn.execute(
            "UPDATE tokens SET expired_time = -1 WHERE id = ?1",
            params![id],
        )
        .unwrap();
        let ctx = repo.validate_token("sk-expired", None).unwrap();
        assert!(ctx.is_some(), "token with expired_time=-1 should always validate");
    }

    #[test]
    fn test_validate_token_user_disabled() {
        let conn = setup();
        let user_repo = UserRepo::new(&conn);
        let repo = TokenRepo::new(&conn);
        repo.create_token(1, "sk-user-disabled", None, 1000, false).unwrap();

        user_repo.update_status(1, 2).unwrap();
        let ctx = repo.validate_token("sk-user-disabled", None).unwrap();
        assert!(ctx.is_none(), "token whose user is disabled must not validate");

        user_repo.update_status(1, 1).unwrap();
        let ctx = repo.validate_token("sk-user-disabled", None).unwrap();
        assert!(ctx.is_some(), "re-enabling user should validate token again");
    }

    #[test]
    fn test_validate_token_ip_whitelist_allowed() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-ip-ok", None, 1000, false).unwrap();
        conn.execute(
            "UPDATE tokens SET allow_ips = '127.0.0.1,10.0.0.5' WHERE id = ?1",
            params![id],
        )
        .unwrap();

        // matching IP → valid
        let ctx = repo.validate_token("sk-ip-ok", Some("127.0.0.1")).unwrap();
        assert!(ctx.is_some());

        let ctx = repo.validate_token("sk-ip-ok", Some("10.0.0.5")).unwrap();
        assert!(ctx.is_some());
    }

    #[test]
    fn test_validate_token_ip_not_in_whitelist() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-ip-deny", None, 1000, false).unwrap();
        conn.execute(
            "UPDATE tokens SET allow_ips = '127.0.0.1' WHERE id = ?1",
            params![id],
        )
        .unwrap();

        // wrong IP → invalid
        let ctx = repo.validate_token("sk-ip-deny", Some("8.8.8.8")).unwrap();
        assert!(ctx.is_none(), "IP not in whitelist must not validate");

        // missing IP when whitelist active → invalid
        let ctx = repo.validate_token("sk-ip-deny", None).unwrap();
        assert!(ctx.is_none(), "missing client_ip with whitelist active must not validate");
    }

    #[test]
    fn test_validate_token_no_whitelist_ignores_ip() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        repo.create_token(1, "sk-no-wl", None, 1000, false).unwrap();

        // no allow_ips set → any IP (or none) should work
        assert!(repo.validate_token("sk-no-wl", None).unwrap().is_some());
        assert!(repo.validate_token("sk-no-wl", Some("1.2.3.4")).unwrap().is_some());
    }

    #[test]
    fn test_real_db_error_surfaces_as_storage_error() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        conn.execute("DROP TABLE tokens", []).unwrap();
        let result = repo.get_token_by_key("whatever");
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Storage(_) => {}
            other => panic!("expected ProxyError::Storage, got {:?}", other),
        }
    }

    #[test]
    fn test_token_with_unlimited_quota_flag() {
        let conn = setup();
        let repo = TokenRepo::new(&conn);
        let id = repo.create_token(1, "sk-unlimited", None, 0, true).unwrap();
        let t = repo.get_token_by_id(id).unwrap().unwrap();
        assert!(t.unlimited_quota);
        assert_eq!(t.remain_quota, 0);

        let ctx = repo.validate_token("sk-unlimited", None).unwrap();
        assert!(ctx.is_some());
        assert!(ctx.unwrap().token.unlimited_quota);
    }
}
