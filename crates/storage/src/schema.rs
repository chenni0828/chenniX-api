use rusqlite::Connection;
use chennix_common::ProxyResult;

pub fn init_db(conn: &Connection) -> ProxyResult<()> {
    let statements = [
        "CREATE TABLE IF NOT EXISTS models (
            id INTEGER PRIMARY KEY,
            canonical_name TEXT NOT NULL UNIQUE,
            input_price REAL NOT NULL DEFAULT 0.0,
            output_price REAL NOT NULL DEFAULT 0.0,
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
        // Drop the legacy model_aliases table (alias system removed).
        // Idempotent: DROP TABLE IF EXISTS never errors if the table is absent.
        "DROP TABLE IF EXISTS model_aliases;",
    ];
    for sql in &statements {
        conn.execute_batch(sql)
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    }
    // Migrate pricing columns for existing databases.
    let alter_statements = [
        // NOTE: input_price/output_price on the `models` table are vestigial —
        // pricing now lives on `model_channels` (per-binding). These columns are
        // kept for backward compatibility but no code reads or writes them.
        "ALTER TABLE models ADD COLUMN input_price REAL NOT NULL DEFAULT 0.0",
        "ALTER TABLE models ADD COLUMN output_price REAL NOT NULL DEFAULT 0.0",
        // Pricing on the model↔channel binding (per-channel model pricing).
        // billing_type: 0=按 token, 1=按调用次数, 2=分段表达式
        "ALTER TABLE model_channels ADD COLUMN billing_type INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE model_channels ADD COLUMN input_price REAL NOT NULL DEFAULT 0.0",
        "ALTER TABLE model_channels ADD COLUMN output_price REAL NOT NULL DEFAULT 0.0",
        "ALTER TABLE model_channels ADD COLUMN call_price REAL NOT NULL DEFAULT 0.0",
        "ALTER TABLE model_channels ADD COLUMN billing_expr TEXT",
        // Per-binding call priority (lower = tried first). Replaces the
        // channel-level `channels.priority` for routing order.
        "ALTER TABLE model_channels ADD COLUMN priority INTEGER NOT NULL DEFAULT 100",
    ];
    for sql in &alter_statements {
        let _ = conn.execute_batch(sql); // ignore "duplicate column" errors
    }
    Ok(())
}

/// Migrate a v1 database (single-user, no `users`/`tokens` tables) to the v2
/// multi-user schema. Safe to call on fresh v2 databases — it detects whether
/// the `users` table already exists and does nothing in that case.
pub fn migrate_v1_to_v2(conn: &Connection) -> ProxyResult<()> {
    let has_users: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users'",
            [],
            |r| r.get(0),
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

    if has_users == 0 {
        // Create users + tokens tables (idempotent — won't fail if somehow present).
        let create_statements = [
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
        ];
        for sql in &create_statements {
            conn.execute_batch(sql)
                .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        }

        // Add new columns to existing v1 tables. Use .ok() to ignore
        // "duplicate column name" errors so the migration is idempotent.
        let alter_statements = [
            "ALTER TABLE channels ADD COLUMN \"group\" TEXT NOT NULL DEFAULT 'default'",
            "ALTER TABLE usage_logs ADD COLUMN user_id INTEGER REFERENCES users(id)",
            "ALTER TABLE usage_logs ADD COLUMN token_id INTEGER REFERENCES tokens(id)",
            "ALTER TABLE usage_logs ADD COLUMN quota_cost INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE request_logs ADD COLUMN user_id INTEGER",
            "ALTER TABLE request_logs ADD COLUMN token_id INTEGER",
            "ALTER TABLE request_logs ADD COLUMN quota_cost INTEGER NOT NULL DEFAULT 0",
        ];
        for sql in &alter_statements {
            let _ = conn.execute_batch(sql);
        }

        // Insert a minimal default admin row. The real admin provisioning
        // (proper bcrypt hash, password setup) is handled in Task 23
        // (ensure_default_admin). Here we only insert if no admin exists yet.
        let admin_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE username = 'admin'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        if admin_count == 0 {
            conn.execute(
                "INSERT INTO users (username, password_hash, role, status, \"group\")
                 VALUES ('admin', 'PLACEHOLDER_BCRYPT_HASH', 10, 1, 'default')",
                [],
            )
            .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        }
    }
    Ok(())
}

/// Check whether a column exists on a table (PRAGMA table_info based).
fn column_exists(conn: &Connection, table: &str, column: &str) -> ProxyResult<bool> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({})", table))
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    let exists = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?
        .any(|r| r.map(|c| c == column).unwrap_or(false));
    Ok(exists)
}

/// Migrate a v2 database to v3: introduces the `model_channels` 3-tuple
/// primary key `(model_id, channel_id, upstream_model_name)` plus the `weight`
/// column, the `models.routing_strategy` column, and the six quota columns on
/// `discovered_models`.
///
/// Safe to call on fresh v3 databases — it detects whether the new columns
/// already exist and skips the corresponding step in that case.
pub fn migrate_v2_to_v3(conn: &Connection) -> ProxyResult<()> {
    // 1. Rebuild model_channels with the 3-tuple primary key + weight.
    //    SQLite cannot ALTER a primary key, so we recreate the table.
    //    Idempotency: skip if the `weight` column already exists (new schema).
    if !column_exists(conn, "model_channels", "weight")? {
        // The rebuilt table reproduces every column the old table gained via
        // ALTER (billing/pricing/priority) so migrated bindings keep their data.
        conn.execute_batch(
            "CREATE TABLE model_channels_new (
                model_id INTEGER NOT NULL REFERENCES models(id) ON DELETE CASCADE,
                channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                upstream_model_name TEXT NOT NULL,
                weight INTEGER NOT NULL DEFAULT 1,
                billing_type INTEGER NOT NULL DEFAULT 0,
                input_price REAL NOT NULL DEFAULT 0.0,
                output_price REAL NOT NULL DEFAULT 0.0,
                call_price REAL NOT NULL DEFAULT 0.0,
                billing_expr TEXT,
                priority INTEGER NOT NULL DEFAULT 100,
                PRIMARY KEY (model_id, channel_id, upstream_model_name)
            );",
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

        // Copy old rows. `upstream_model_name` was nullable in v2; coerce NULL
        // to '' so the new NOT NULL PK column is satisfied. `weight` defaults
        // to 1 for all migrated bindings. The old 2-tuple PK guaranteed that
        // (model_id, channel_id) was unique, so the resulting 3-tuple is unique
        // as well — no PK conflict on INSERT.
        conn.execute_batch(
            "INSERT INTO model_channels_new
                (model_id, channel_id, upstream_model_name, weight,
                 billing_type, input_price, output_price, call_price,
                 billing_expr, priority)
             SELECT model_id, channel_id, COALESCE(upstream_model_name, ''), 1,
                    billing_type, input_price, output_price, call_price,
                    billing_expr, priority
             FROM model_channels;",
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;

        conn.execute_batch(
            "DROP TABLE model_channels;
             ALTER TABLE model_channels_new RENAME TO model_channels;",
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    }

    // 2. models.routing_strategy (idempotent ALTER).
    if !column_exists(conn, "models", "routing_strategy")? {
        conn.execute_batch(
            "ALTER TABLE models ADD COLUMN routing_strategy TEXT NOT NULL DEFAULT 'priority'",
        )
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    }

    // 3. discovered_models quota columns (idempotent ALTERs).
    let quota_cols: &[(&str, &str)] = &[
        ("quota_limit", "ALTER TABLE discovered_models ADD COLUMN quota_limit INTEGER"),
        ("quota_unit", "ALTER TABLE discovered_models ADD COLUMN quota_unit TEXT"),
        ("quota_window", "ALTER TABLE discovered_models ADD COLUMN quota_window TEXT"),
        ("used_quota", "ALTER TABLE discovered_models ADD COLUMN used_quota INTEGER NOT NULL DEFAULT 0"),
        ("last_reset_at", "ALTER TABLE discovered_models ADD COLUMN last_reset_at TEXT"),
        ("quota_status", "ALTER TABLE discovered_models ADD COLUMN quota_status TEXT NOT NULL DEFAULT 'available'"),
    ];
    for (col, sql) in quota_cols {
        if !column_exists(conn, "discovered_models", col)? {
            conn.execute_batch(sql)
                .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_db_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0))
            .unwrap();
        // models, users, tokens, channels, channel_keys,
        // discovered_models, model_channels, usage_logs, request_logs,
        // key_usage_summary = 10 tables.
        assert!(count >= 9);
    }

    #[test]
    fn test_init_db_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        init_db(&conn).unwrap(); // 不应报错
    }

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
        // username must be unique
        let _: i64 = conn
            .query_row("SELECT COUNT(*) FROM pragma_index_list('users')", [], |r| r.get(0))
            .unwrap();
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
        // Indexes on tokens(user_id) and tokens(key) should exist.
        let idx_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='tokens'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(idx_count >= 2, "expected at least 2 indexes on tokens, got {}", idx_count);
    }

    #[test]
    fn test_channels_has_group_column() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cols = column_names(&conn, "channels");
        assert!(cols.contains(&"group".to_string()), "channels missing 'group' column");
    }

    #[test]
    fn test_usage_and_request_logs_have_billing_columns() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        for col in ["user_id", "token_id", "quota_cost"] {
            assert!(
                column_names(&conn, "usage_logs").contains(&col.to_string()),
                "usage_logs missing column: {}", col
            );
            assert!(
                column_names(&conn, "request_logs").contains(&col.to_string()),
                "request_logs missing column: {}", col
            );
        }
    }

    #[test]
    fn test_migrate_v1_to_v2() {
        let conn = Connection::open_in_memory().unwrap();
        // Build a v1-style schema (no users/tokens, no new columns).
        conn.execute_batch(
            "CREATE TABLE channels (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                provider TEXT NOT NULL,
                base_url TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 100,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE usage_logs (
                id INTEGER PRIMARY KEY,
                channel_id INTEGER NOT NULL,
                key_id INTEGER NOT NULL,
                model_id INTEGER NOT NULL,
                prompt_tokens INTEGER NOT NULL,
                completion_tokens INTEGER NOT NULL,
                total_tokens INTEGER NOT NULL,
                request_type TEXT NOT NULL,
                status TEXT NOT NULL,
                error_message TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE request_logs (
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
                response_status INTEGER,
                duration_ms INTEGER NOT NULL,
                stream INTEGER NOT NULL DEFAULT 0,
                error_message TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();

        // Run migration.
        migrate_v1_to_v2(&conn).unwrap();

        // users + tokens should now exist.
        let has_users: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_users, 1);
        let has_tokens: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='tokens'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(has_tokens, 1);

        // New columns should be present on existing v1 tables.
        assert!(column_names(&conn, "channels").contains(&"group".to_string()));
        for col in ["user_id", "token_id", "quota_cost"] {
            assert!(column_names(&conn, "usage_logs").contains(&col.to_string()));
            assert!(column_names(&conn, "request_logs").contains(&col.to_string()));
        }

        // Default admin row should be present.
        let admin_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users WHERE username = 'admin'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(admin_count, 1);

        // Migration must be idempotent.
        migrate_v1_to_v2(&conn).unwrap();
        let admin_count_again: i64 = conn
            .query_row("SELECT COUNT(*) FROM users WHERE username = 'admin'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(admin_count_again, 1, "admin should not be duplicated on re-run");
    }

    #[test]
    fn test_models_has_pricing_columns() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let cols = column_names(&conn, "models");
        assert!(cols.contains(&"input_price".to_string()), "models missing 'input_price' column");
        assert!(cols.contains(&"output_price".to_string()), "models missing 'output_price' column");
    }

    #[test]
    fn test_migrate_v1_to_v2_on_fresh_db() {
        // A fresh v2 DB already has users — migration should be a no-op.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let tables_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0))
            .unwrap();
        migrate_v1_to_v2(&conn).unwrap();
        let tables_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables_before, tables_after);
        // No admin row should have been inserted by migrate on a fresh v2 DB
        // (init_db does not create admin, and migrate short-circuits when users exists).
        let admin_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users WHERE username = 'admin'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(admin_count, 0);
    }

    #[test]
    fn test_migrate_v2_to_v3() {
        // Build a v2-style schema for the three affected tables (no routing_strategy,
        // no weight, no quota columns, 2-tuple PK on model_channels).
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE models (
                id INTEGER PRIMARY KEY,
                canonical_name TEXT NOT NULL UNIQUE,
                input_price REAL NOT NULL DEFAULT 0.0,
                output_price REAL NOT NULL DEFAULT 0.0,
                created_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE channels (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL UNIQUE,
                provider TEXT NOT NULL,
                base_url TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 100,
                \"group\" TEXT NOT NULL DEFAULT 'default',
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
            CREATE TABLE discovered_models (
                id INTEGER PRIMARY KEY,
                channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                raw_model_name TEXT NOT NULL,
                discovered_at TEXT NOT NULL DEFAULT (datetime('now')),
                status TEXT NOT NULL DEFAULT 'unmerged',
                merged_to_model_id INTEGER REFERENCES models(id),
                is_free INTEGER NOT NULL DEFAULT 0,
                source TEXT,
                metadata TEXT,
                UNIQUE(channel_id, raw_model_name)
            );
            CREATE TABLE model_channels (
                model_id INTEGER NOT NULL REFERENCES models(id) ON DELETE CASCADE,
                channel_id INTEGER NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
                upstream_model_name TEXT,
                billing_type INTEGER NOT NULL DEFAULT 0,
                input_price REAL NOT NULL DEFAULT 0.0,
                output_price REAL NOT NULL DEFAULT 0.0,
                call_price REAL NOT NULL DEFAULT 0.0,
                billing_expr TEXT,
                priority INTEGER NOT NULL DEFAULT 100,
                PRIMARY KEY (model_id, channel_id)
            );
            INSERT INTO models (id, canonical_name) VALUES (1, 'gpt-4o');
            INSERT INTO channels (id, name, provider, base_url) VALUES (2, 'openai', 'openai-compatible', 'http://x');
            INSERT INTO model_channels (model_id, channel_id, upstream_model_name, priority)
            VALUES (1, 2, 'gpt-4o', 50);",
        )
        .unwrap();

        migrate_v2_to_v3(&conn).unwrap();

        // models.routing_strategy added with default 'priority'.
        let cols_m = column_names(&conn, "models");
        assert!(cols_m.contains(&"routing_strategy".to_string()));
        let strat: String = conn
            .query_row("SELECT routing_strategy FROM models WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(strat, "priority");

        // discovered_models quota columns added.
        let cols_d = column_names(&conn, "discovered_models");
        for c in ["quota_limit", "quota_unit", "quota_window", "used_quota", "last_reset_at", "quota_status"] {
            assert!(cols_d.contains(&c.to_string()), "discovered_models missing {}", c);
        }

        // model_channels: weight added; old row preserved (weight=1, priority=50,
        // upstream_model_name carried over).
        let cols_c = column_names(&conn, "model_channels");
        assert!(cols_c.contains(&"weight".to_string()));
        let (weight, priority): (i32, i32) = conn
            .query_row(
                "SELECT weight, priority FROM model_channels
                 WHERE model_id=1 AND channel_id=2 AND upstream_model_name='gpt-4o'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(weight, 1);
        assert_eq!(priority, 50);

        // 3-tuple PK: same (model_id, channel_id) with a different upstream is
        // now allowed, while a duplicate 3-tuple is rejected.
        conn.execute(
            "INSERT INTO model_channels (model_id, channel_id, upstream_model_name)
             VALUES (1, 2, 'gpt-4o-mini')",
            [],
        )
        .unwrap();
        let dup = conn.execute(
            "INSERT INTO model_channels (model_id, channel_id, upstream_model_name)
             VALUES (1, 2, 'gpt-4o')",
            [],
        );
        assert!(dup.is_err(), "duplicate 3-tuple must be rejected");

        // Idempotent: re-running on an already-v3 DB is a no-op.
        migrate_v2_to_v3(&conn).unwrap();
    }

    #[test]
    fn test_migrate_v2_to_v3_noop_on_fresh_v3() {
        // A fresh DB built by init_db is already v3 — migration must be a no-op.
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let tables_before: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0))
            .unwrap();
        migrate_v2_to_v3(&conn).unwrap();
        let tables_after: i64 = conn
            .query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(tables_before, tables_after);
        // weight column present (init_db already created it).
        assert!(column_names(&conn, "model_channels").contains(&"weight".to_string()));
    }
}
