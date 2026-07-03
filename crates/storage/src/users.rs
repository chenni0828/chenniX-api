use chennix_common::{ProxyError, ProxyResult, UserConfig};
use rusqlite::{params, Connection, OptionalExtension};

pub struct UserRepo<'a> {
    conn: &'a Connection,
}

impl<'a> UserRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// 返回 users 表的总用户数（用于 setup wizard 判断是否需要初始化）。
    pub fn count_users(&self) -> ProxyResult<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .map_err(|e| ProxyError::Storage(e.to_string()))
    }

    pub fn create_user(
        &self,
        username: &str,
        password_hash: &str,
        role: i32,
        group: &str,
    ) -> ProxyResult<i64> {
        self.conn
            .execute(
                "INSERT INTO users (username, password_hash, role, status, \"group\")
                 VALUES (?1, ?2, ?3, 1, ?4)",
                params![username, password_hash, role, group],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn get_user_by_id(&self, id: i64) -> ProxyResult<Option<UserConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, username, role, status, quota, used_quota, \"group\"
                 FROM users WHERE id = ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<UserConfig> = stmt
            .query_row(params![id], map_user_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn get_user_by_username(&self, username: &str) -> ProxyResult<Option<UserConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, username, role, status, quota, used_quota, \"group\"
                 FROM users WHERE username = ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<UserConfig> = stmt
            .query_row(params![username], map_user_row)
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    pub fn update_quota(&self, id: i64, new_quota: i64) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE users SET quota = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![new_quota, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Atomically increment `used_quota` by `delta` (use negative delta to decrement).
    pub fn update_used_quota_delta(&self, id: i64, delta: i64) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE users SET used_quota = used_quota + ?1, updated_at = datetime('now')
                 WHERE id = ?2",
                params![delta, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn list_users(&self) -> ProxyResult<Vec<UserConfig>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, username, role, status, quota, used_quota, \"group\"
                 FROM users ORDER BY id",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], map_user_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    pub fn update_status(&self, id: i64, status: i32) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE users SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![status, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn delete_user(&self, id: i64) -> ProxyResult<()> {
        self.conn
            .execute("DELETE FROM users WHERE id = ?1", params![id])
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    pub fn get_quota(&self, id: i64) -> ProxyResult<Option<i64>> {
        let mut stmt = self
            .conn
            .prepare("SELECT quota FROM users WHERE id = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<i64> = stmt
            .query_row(params![id], |r| r.get(0))
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    // ===== Admin API methods =====

    /// Create a user with an explicit quota value.
    ///
    /// This is the admin-panel variant of `create_user` — it allows setting
    /// the initial `quota` directly (the base `create_user` defaults quota to 0).
    pub fn create_user_with_quota(
        &self,
        username: &str,
        password_hash: &str,
        role: i32,
        group: &str,
        quota: i64,
    ) -> ProxyResult<i64> {
        self.conn
            .execute(
                "INSERT INTO users (username, password_hash, role, status, quota, used_quota, \"group\")
                 VALUES (?1, ?2, ?3, 1, ?4, 0, ?5)",
                params![username, password_hash, role, quota, group],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a user's profile fields (username, role, status, group, quota).
    ///
    /// `used_quota` is **not** touched — it is managed by the billing layer.
    /// `password_hash` is **not** touched — use `update_password` for that.
    pub fn update_user(
        &self,
        id: i64,
        username: &str,
        role: i32,
        status: i32,
        group: &str,
        quota: i64,
    ) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE users
                 SET username = ?1, role = ?2, status = ?3, \"group\" = ?4,
                     quota = ?5, updated_at = datetime('now')
                 WHERE id = ?6",
                params![username, role, status, group, quota, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Update only the password hash for a user.
    ///
    /// The caller is responsible for bcrypt-hashing the plaintext password
    /// before calling this method.
    pub fn update_password(&self, id: i64, password_hash: &str) -> ProxyResult<()> {
        self.conn
            .execute(
                "UPDATE users SET password_hash = ?1, updated_at = datetime('now')
                 WHERE id = ?2",
                params![password_hash, id],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(())
    }

    /// Get the password hash for a user by username.
    ///
    /// Used by the admin login handler to verify credentials. Returns
    /// `None` if the user does not exist.
    pub fn get_password_hash(&self, username: &str) -> ProxyResult<Option<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT password_hash FROM users WHERE username = ?1")
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let row: Option<String> = stmt
            .query_row(params![username], |r| r.get(0))
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }
}

fn map_user_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<UserConfig> {
    Ok(UserConfig {
        id: r.get(0)?,
        username: r.get(1)?,
        role: r.get(2)?,
        status: r.get(3)?,
        quota: r.get(4)?,
        used_quota: r.get(5)?,
        group: r.get(6)?,
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
    fn test_create_and_get_user() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        let id = repo.create_user("alice", "hash", 1, "default").unwrap();
        assert!(id > 0);

        let by_id = repo.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(by_id.username, "alice");
        assert_eq!(by_id.role, 1);
        assert_eq!(by_id.status, 1);
        assert_eq!(by_id.quota, 0);
        assert_eq!(by_id.used_quota, 0);
        assert_eq!(by_id.group, "default");

        let by_name = repo.get_user_by_username("alice").unwrap().unwrap();
        assert_eq!(by_name.id, id);
    }

    #[test]
    fn test_get_user_missing_returns_none() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        assert!(repo.get_user_by_id(99999).unwrap().is_none());
        assert!(repo.get_user_by_username("nobody").unwrap().is_none());
        assert!(repo.get_quota(99999).unwrap().is_none());
    }

    #[test]
    fn test_update_quota() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        let id = repo.create_user("bob", "hash", 1, "default").unwrap();
        repo.update_quota(id, 5000).unwrap();
        assert_eq!(repo.get_quota(id).unwrap(), Some(5000));
        let u = repo.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(u.quota, 5000);
        assert_eq!(u.used_quota, 0);
    }

    #[test]
    fn test_update_used_quota_delta_atomic() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        let id = repo.create_user("carol", "hash", 1, "default").unwrap();
        repo.update_quota(id, 1000).unwrap();

        repo.update_used_quota_delta(id, 100).unwrap();
        repo.update_used_quota_delta(id, 50).unwrap();
        let u = repo.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(u.used_quota, 150);
        assert_eq!(u.remaining_quota(), 850);

        // negative delta
        repo.update_used_quota_delta(id, -30).unwrap();
        let u = repo.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(u.used_quota, 120);
    }

    #[test]
    fn test_list_users() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        assert_eq!(repo.list_users().unwrap().len(), 0);
        repo.create_user("u1", "h", 1, "default").unwrap();
        repo.create_user("u2", "h", 10, "vip").unwrap();
        let users = repo.list_users().unwrap();
        assert_eq!(users.len(), 2);
        assert_eq!(users[0].username, "u1");
        assert_eq!(users[1].username, "u2");
        assert_eq!(users[1].group, "vip");
    }

    #[test]
    fn test_update_status_and_delete() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        let id = repo.create_user("dave", "h", 1, "default").unwrap();

        repo.update_status(id, 2).unwrap();
        let u = repo.get_user_by_id(id).unwrap().unwrap();
        assert_eq!(u.status, 2);
        assert!(!u.is_enabled());

        repo.update_status(id, 1).unwrap();
        let u = repo.get_user_by_id(id).unwrap().unwrap();
        assert!(u.is_enabled());

        repo.delete_user(id).unwrap();
        assert!(repo.get_user_by_id(id).unwrap().is_none());
    }

    #[test]
    fn test_username_unique_constraint() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        repo.create_user("alice", "h", 1, "default").unwrap();
        let result = repo.create_user("alice", "h2", 1, "default");
        assert!(result.is_err(), "expected unique constraint error");
    }

    #[test]
    fn test_real_db_error_surfaces_as_storage_error() {
        let conn = setup();
        let repo = UserRepo::new(&conn);
        conn.execute("DROP TABLE users", []).unwrap();
        let result = repo.get_user_by_id(1);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Storage(_) => {}
            other => panic!("expected ProxyError::Storage, got {:?}", other),
        }
    }
}
