use rusqlite::Connection;
use chennix_common::ProxyResult;

pub fn init_db(conn: &Connection) -> ProxyResult<()> {
    // 在任何 CREATE TABLE 之前检测是否为全新库。
    // 判据：models 表不存在（models 是 init_db 创建的第一张业务表）。
    //
    // 全新库会在末尾直接写入 schema_version=CURRENT_SCHEMA_VERSION，
    // 让 run_migrations 跳过版本校验。老库（models 表已存在）不写 marker，
    // 由 run_migrations 校验版本号是否匹配。
    //
    // 这个检测必须前置——一旦 CREATE TABLE IF NOT EXISTS 执行后，老库和
    // 新库的 sqlite_master 就无法区分了。
    let is_fresh_db = {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master \
                 WHERE type='table' AND name='models'",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        count == 0
    };

    let statements = [
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            canonical_name TEXT NOT NULL UNIQUE,
            routing_strategy TEXT NOT NULL DEFAULT 'priority',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        "CREATE TABLE IF NOT EXISTS users (
            id INTEGER PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            role INTEGER NOT NULL DEFAULT 1,
            status INTEGER NOT NULL DEFAULT 1,
            quota INTEGER NOT NULL DEFAULT 0,
            used_quota INTEGER NOT NULL DEFAULT 0,
            \"group\" TEXT NOT NULL DEFAULT 'default',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        "CREATE TABLE IF NOT EXISTS tokens (
            id INTEGER PRIMARY KEY,
            user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
            key TEXT NOT NULL UNIQUE,
            name TEXT,
            remain_quota INTEGER NOT NULL DEFAULT 0,
            used_quota INTEGER NOT NULL DEFAULT 0,
            unlimited_quota INTEGER NOT NULL DEFAULT 0,
            expired_time INTEGER NOT NULL DEFAULT -1,
            model_limits_enabled INTEGER NOT NULL DEFAULT 0,
            model_limits TEXT,
            status INTEGER NOT NULL DEFAULT 1,
            allow_ips TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        "CREATE INDEX IF NOT EXISTS idx_tokens_user ON tokens(user_id);",
        "CREATE INDEX IF NOT EXISTS idx_tokens_key ON tokens(key);",
        "CREATE TABLE IF NOT EXISTS channels (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            provider TEXT NOT NULL,
            base_url TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 100,
            \"group\" TEXT NOT NULL DEFAULT 'default',
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        "CREATE TABLE IF NOT EXISTS channel_keys (
            id INTEGER PRIMARY KEY,
            channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
            api_key TEXT NOT NULL,
            label TEXT,
            cost_tier TEXT NOT NULL DEFAULT 'paid',
            key_priority INTEGER NOT NULL DEFAULT 100,
            price_per_1k_tokens REAL,
            free_quota INTEGER,
            used_quota INTEGER NOT NULL DEFAULT 0,
            quota_reset_period TEXT,
            status TEXT NOT NULL DEFAULT 'active',
            cooldown_until TEXT,
            consecutive_failures INTEGER NOT NULL DEFAULT 0,
            balance_api_url TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        "CREATE INDEX IF NOT EXISTS idx_channel_keys_channel ON channel_keys(channel_id);",
        "CREATE TABLE IF NOT EXISTS discovered_models (
            id INTEGER PRIMARY KEY,
            channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
            raw_model_name TEXT NOT NULL,
            discovered_at TEXT NOT NULL DEFAULT (datetime('now')),
            status TEXT NOT NULL DEFAULT 'unmerged',
            merged_to_model_id INTEGER REFERENCES models(id),
            is_free INTEGER NOT NULL DEFAULT 0,
            source TEXT,
            metadata TEXT,
            quota_limit INTEGER,
            quota_unit TEXT,
            quota_window TEXT,
            used_quota INTEGER NOT NULL DEFAULT 0,
            last_reset_at TEXT,
            quota_status TEXT NOT NULL DEFAULT 'available',
            UNIQUE(channel_id, raw_model_name)
        );",
        "CREATE TABLE IF NOT EXISTS model_channels (
            model_id INTEGER NOT NULL REFERENCES models(id) ON DELETE CASCADE,
            channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
            upstream_model_name TEXT NOT NULL,
            weight INTEGER NOT NULL DEFAULT 1,
            -- Per-binding pricing. billing_type: 0=按 token, 1=按调用次数, 2=分段表达式
            billing_type INTEGER NOT NULL DEFAULT 0,
            input_price REAL NOT NULL DEFAULT 0.0,
            output_price REAL NOT NULL DEFAULT 0.0,
            call_price REAL NOT NULL DEFAULT 0.0,
            billing_expr TEXT,
            -- Per-binding call priority (lower = tried first)
            priority INTEGER NOT NULL DEFAULT 100,
            PRIMARY KEY (model_id, channel_id, upstream_model_name)
        );",
        "CREATE TABLE IF NOT EXISTS usage_logs (
            id INTEGER PRIMARY KEY,
            channel_id INTEGER NOT NULL REFERENCES channels(id),
            key_id INTEGER NOT NULL REFERENCES channel_keys(id),
            model_id INTEGER NOT NULL REFERENCES models(id),
            user_id INTEGER REFERENCES users(id),
            token_id INTEGER REFERENCES tokens(id),
            quota_cost INTEGER NOT NULL DEFAULT 0,
            prompt_tokens INTEGER NOT NULL,
            completion_tokens INTEGER NOT NULL,
            total_tokens INTEGER NOT NULL,
            request_type TEXT NOT NULL,
            status TEXT NOT NULL,
            error_message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        "CREATE INDEX IF NOT EXISTS idx_usage_logs_key ON usage_logs(key_id, created_at);",
        "CREATE TABLE IF NOT EXISTS request_logs (
            id INTEGER PRIMARY KEY,
            request_id TEXT NOT NULL,
            client_ip TEXT,
            method TEXT NOT NULL,
            path TEXT NOT NULL,
            client_model TEXT,
            normalized_model TEXT,
            channel_name TEXT,
            key_label TEXT,
            attempted_keys TEXT,
            upstream_status INTEGER,
            upstream_model TEXT,
            response_status INTEGER,
            duration_ms INTEGER NOT NULL,
            stream INTEGER NOT NULL DEFAULT 0,
            user_id INTEGER,
            token_id INTEGER,
            quota_cost INTEGER NOT NULL DEFAULT 0,
            error_message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
        "CREATE INDEX IF NOT EXISTS idx_request_logs_created ON request_logs(created_at);",
        "CREATE INDEX IF NOT EXISTS idx_request_logs_channel ON request_logs(channel_name);",
        "CREATE TABLE IF NOT EXISTS key_usage_summary (
            key_id INTEGER NOT NULL REFERENCES channel_keys(id) ON DELETE CASCADE,
            period_start TEXT NOT NULL,
            period_end TEXT NOT NULL,
            total_tokens INTEGER NOT NULL DEFAULT 0,
            request_count INTEGER NOT NULL DEFAULT 0,
            PRIMARY KEY (key_id, period_start)
        );",
        // schema_meta: 存储 schema 版本号等元信息。
        "CREATE TABLE IF NOT EXISTS schema_meta (key TEXT PRIMARY KEY, value TEXT);",
    ];
    for sql in &statements {
        conn.execute_batch(sql)
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    }

    // 全新库：直接标记为最新 schema 版本。
    //
    // 项目不做向后兼容——新增 schema 变更时，直接修改 init_db 的建表语句
    // 并递增 CURRENT_SCHEMA_VERSION。老库升级时由 run_migrations 校验
    // 版本号，不匹配则报错（要求删库重建或用对应版本代码）。
    if is_fresh_db {
        conn.execute(
            "INSERT OR REPLACE INTO schema_meta (key, value) \
             VALUES ('schema_version', ?1)",
            rusqlite::params![CURRENT_SCHEMA_VERSION.to_string()],
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        tracing::debug!(
            version = CURRENT_SCHEMA_VERSION,
            "fresh database initialized, schema_version marker set"
        );
    }

    Ok(())
}

/// 当前 schema 版本。`init_db` 创建新库时直接写入此值。
///
/// 项目不做向后兼容：版本号只是一个递增计数器，用于判断数据库是否
/// 与当前代码匹配。数字本身没有语义（不对应具体的表结构特征）。
/// 每次 `init_db` 的建表语句有变更时递增此常量。
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// 读取数据库的 schema 版本号。
///
/// 仅从 `schema_meta.schema_version` marker 读取。新库由 `init_db`
/// 直接写入最新版本号；老库若无此 marker，返回 0 表示版本未知，
/// `run_migrations` 会报错。
pub fn get_current_schema_version(conn: &Connection) -> ProxyResult<u32> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key='schema_version'",
            [],
            |r| r.get(0),
        )
        .ok();
    Ok(value.and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// 校验数据库 schema 版本是否匹配代码版本。
///
/// 不做迁移——项目面向未来，不做向后兼容。版本不匹配直接报错，
/// 让用户用对应版本的代码或删库重建。
pub fn run_migrations(conn: &Connection) -> ProxyResult<()> {
    let current = get_current_schema_version(conn)?;
    let target = CURRENT_SCHEMA_VERSION;

    if current != target {
        return Err(chennix_common::ProxyError::Storage(format!(
            "schema version mismatch: database is v{}, code expects v{}. \
             This project does not support backward compatibility. \
             Either use the matching code version or delete the database \
             to reinitialize.",
            current, target
        )));
    }

    tracing::debug!(current = current, "schema version OK");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column_names(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({})", table)).unwrap();
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(|r| r.unwrap())
            .collect::<Vec<_>>();
        rows
    }

    #[test]
    fn test_init_db_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0))
            .unwrap();
        // models, users, tokens, channels, channel_keys,
        // discovered_models, model_channels, usage_logs, request_logs,
        // key_usage_summary, schema_meta = 11 tables.
        assert!(count >= 10);
    }

    #[test]
    fn test_init_db_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap(); // 不应报错
    }

    #[test]
    fn test_users_table_exists() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cols = column_names(&conn, "users");
        for expected in [
            "id", "username", "password_hash", "role", "status", "quota",
            "used_quota", "group", "created_at", "updated_at",
        ] {
            assert!(cols.contains(&expected.to_string()), "users missing column: {}", expected);
        }
    }

    #[test]
    fn test_tokens_table_exists() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cols = column_names(&conn, "tokens");
        for expected in [
            "id", "user_id", "key", "name", "remain_quota", "used_quota",
            "unlimited_quota", "expired_time", "model_limits_enabled",
            "model_limits", "status", "allow_ips", "created_at", "updated_at",
        ] {
            assert!(cols.contains(&expected.to_string()), "tokens missing column: {}", expected);
        }
    }

    #[test]
    fn test_channels_has_group_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cols = column_names(&conn, "channels");
        assert!(cols.contains(&"group".to_string()));
    }

    #[test]
    fn test_usage_and_request_logs_have_billing_columns() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for col in ["user_id", "token_id", "quota_cost"] {
            assert!(column_names(&conn, "usage_logs").contains(&col.to_string()));
            assert!(column_names(&conn, "request_logs").contains(&col.to_string()));
        }
    }

    #[test]
    fn test_model_channels_has_pricing_columns() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cols = column_names(&conn, "model_channels");
        for expected in [
            "billing_type", "input_price", "output_price", "call_price",
            "billing_expr", "priority", "weight",
        ] {
            assert!(cols.contains(&expected.to_string()), "model_channels missing: {}", expected);
        }
    }

    /// 新库 init_db 后 schema_version 应为 CURRENT_SCHEMA_VERSION。
    #[test]
    fn test_fresh_db_gets_latest_schema_version() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let v = get_current_schema_version(&conn).unwrap();
        assert_eq!(v, CURRENT_SCHEMA_VERSION);
    }

    /// run_migrations 对版本匹配的库应直接通过（no-op）。
    #[test]
    fn test_run_migrations_on_matching_version() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        run_migrations(&conn).unwrap(); // 版本匹配，直接通过
    }

    /// run_migrations 对版本不匹配的库应报错。
    #[test]
    fn test_run_migrations_rejects_version_mismatch() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // 篡改版本号模拟不匹配的数据库
        conn.execute(
            "UPDATE schema_meta SET value='99' WHERE key='schema_version'",
            [],
        )
        .unwrap();
        let err = run_migrations(&conn).unwrap_err();
        match err {
            chennix_common::ProxyError::Storage(msg) => {
                assert!(msg.contains("version mismatch"), "unexpected error: {}", msg);
                assert!(msg.contains("v99"), "unexpected error: {}", msg);
                assert!(
                    msg.contains(&format!("v{}", CURRENT_SCHEMA_VERSION)),
                    "unexpected error: {}", msg
                );
            }
            _ => panic!("expected Storage error, got {:?}", err),
        }
    }

    /// 无 schema_version marker 的库（模拟未知老库）应被识别为 v0 并报错。
    #[test]
    fn test_run_migrations_rejects_unknown_db() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        // 删除 marker 模拟无版本号的老库
        conn.execute("DELETE FROM schema_meta WHERE key='schema_version'", [])
            .unwrap();
        let v = get_current_schema_version(&conn).unwrap();
        assert_eq!(v, 0);
        let err = run_migrations(&conn).unwrap_err();
        match err {
            chennix_common::ProxyError::Storage(msg) => {
                assert!(msg.contains("v0"), "unexpected error: {}", msg);
            }
            _ => panic!("expected Storage error, got {:?}", err),
        }
    }
}
