//! Task 23: Application configuration + default admin provisioning.
//!
//! Config is loaded from a YAML file at startup. The schema extends the
//! spec's minimal `AppConfig` with a `database` section (DB path) — this
//! is a necessary addition because the server needs to know where to
//! open the SQLite database.

use chennix_common::{ProxyError, ProxyResult};
use rusqlite::Connection;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub log: LogConfig,
    pub bootstrap: BootstrapConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub tls: TlsConfig,
    /// 非流式上游请求整体超时（秒）。默认 60。
    #[serde(default = "default_upstream_timeout")]
    pub upstream_timeout_secs: u64,
    /// 流式请求首字节到达超时（秒）。默认 300。不中断已建立的流。
    #[serde(default = "default_streaming_timeout")]
    pub streaming_timeout_secs: u64,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}

fn default_port() -> u16 {
    8080
}

fn default_upstream_timeout() -> u64 {
    60
}

fn default_streaming_timeout() -> u64 {
    300
}

#[derive(Debug, Default, Deserialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    #[allow(dead_code)]
    pub cert: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub key: String,
}

#[derive(Debug, Deserialize)]
pub struct LogConfig {
    #[serde(default = "default_log_level")]
    pub level: String,
}

fn default_log_level() -> String {
    "info".to_string()
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct BootstrapConfig {
    pub config_file: String,
}

#[derive(Debug, Deserialize)]
pub struct DatabaseConfig {
    #[serde(default = "default_db_path")]
    pub path: String,
}

fn default_db_path() -> String {
    // 默认 /data/chennix.db 适配容器化部署（挂载 /data 卷持久化）。
    // 本地开发时可通过 config.yaml 或 DB_PATH 环境变量覆盖。
    "/data/chennix.db".to_string()
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
        }
    }
}

/// Load the application config from a YAML file, then apply environment
/// variable overrides for containerized deployment.
///
/// Supported env vars (override YAML values):
/// - `PORT`           → `server.port`
/// - `HOST`           → `server.host`
/// - `DB_PATH`        → `database.path`
/// - `UPSTREAM_TIMEOUT_SECS`  → `server.upstream_timeout_secs`
/// - `STREAMING_TIMEOUT_SECS` → `server.streaming_timeout_secs`
/// - `RUST_LOG`       → `log.level` (also read by tracing_subscriber)
/// - `CHENNIX_ADMIN_PASSWORD` → used by `ensure_default_admin` (see below)
///
/// 容器化部署时通过环境变量覆盖配置，避免修改 config.yaml 文件。
pub fn load_config(path: &str) -> ProxyResult<AppConfig> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| ProxyError::Config(format!("read config {}: {}", path, e)))?;
    let mut config: AppConfig = serde_yaml::from_str(&content)
        .map_err(|e| ProxyError::Config(format!("parse config yaml: {}", e)))?;

    // 环境变量覆盖（优先级高于 YAML）
    if let Ok(port) = std::env::var("PORT") {
        if let Ok(port) = port.parse::<u16>() {
            config.server.port = port;
            tracing::info!("overrode server.port from PORT env: {}", port);
        }
    }
    if let Ok(host) = std::env::var("HOST") {
        config.server.host = host;
        tracing::info!("overrode server.host from HOST env: {}", config.server.host);
    }
    if let Ok(db_path) = std::env::var("DB_PATH") {
        config.database.path = db_path;
        tracing::info!("overrode database.path from DB_PATH env: {}", config.database.path);
    }
    if let Ok(secs) = std::env::var("UPSTREAM_TIMEOUT_SECS") {
        if let Ok(secs) = secs.parse::<u64>() {
            config.server.upstream_timeout_secs = secs;
            tracing::info!("overrode server.upstream_timeout_secs from env: {}", secs);
        }
    }
    if let Ok(secs) = std::env::var("STREAMING_TIMEOUT_SECS") {
        if let Ok(secs) = secs.parse::<u64>() {
            config.server.streaming_timeout_secs = secs;
            tracing::info!("overrode server.streaming_timeout_secs from env: {}", secs);
        }
    }
    if let Ok(level) = std::env::var("RUST_LOG") {
        config.log.level = level;
    }

    Ok(config)
}

/// Create a default admin user if the `users` table is empty AND the
/// `CHENNIX_ADMIN_PASSWORD` environment variable is set.
///
/// 当 `CHENNIX_ADMIN_PASSWORD` 环境变量存在时：
/// - 自动创建 admin 用户（username=admin，password=env var 值）
/// - 用于 CI / 自动化部署场景
///
/// 当 `CHENNIX_ADMIN_PASSWORD` 环境变量不存在时：
/// - **跳过自动创建**，等待用户通过 `/setup` 页面设置管理员账号
/// - 首次访问 web 界面会被引导到 setup wizard
///
/// users 表非空时：幂等 return（无副作用）。
pub fn ensure_default_admin(conn: &Connection) -> ProxyResult<()> {
    let user_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .map_err(|e| ProxyError::Storage(e.to_string()))?;

    if user_count == 0 {
        // 仅当显式设置 CHENNIX_ADMIN_PASSWORD（且非空）时才自动创建
        let admin_password = std::env::var("CHENNIX_ADMIN_PASSWORD")
            .ok()
            .filter(|p| !p.is_empty());
        if let Some(password) = admin_password {
            let password_hash =
                bcrypt::hash(&password, bcrypt::DEFAULT_COST).map_err(|e| {
                    ProxyError::Config(format!("bcrypt hash failed: {}", e))
                })?;

            conn.execute(
                "INSERT INTO users (username, password_hash, role, status, quota, used_quota, \"group\")
                 VALUES (?1, ?2, ?3, 1, ?4, 0, 'default')",
                rusqlite::params!["admin", password_hash, 100, 999_999_999_i64],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;

            tracing::info!("created default admin user (username=admin, password from CHENNIX_ADMIN_PASSWORD env)");
        } else {
            // 未设置环境变量：等待 setup 页面初始化
            tracing::info!(
                "no users found and CHENNIX_ADMIN_PASSWORD not set — \
                 awaiting initialization via /setup page"
            );
        }
        return Ok(());
    }

    // Fix placeholder password hash left by migrate_v1_to_v2.
    let placeholder: Option<String> = conn
        .query_row(
            "SELECT password_hash FROM users WHERE username = 'admin'",
            [],
            |r| r.get(0),
        )
        .ok();

    if placeholder.as_deref() == Some("PLACEHOLDER_BCRYPT_HASH") {
        let password = std::env::var("CHENNIX_ADMIN_PASSWORD")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "admin123".to_string());
        let password_hash =
            bcrypt::hash(&password, bcrypt::DEFAULT_COST).map_err(|e| {
                ProxyError::Config(format!("bcrypt hash failed: {}", e))
            })?;

        conn.execute(
            "UPDATE users SET password_hash = ?1 WHERE username = 'admin'",
            rusqlite::params![password_hash],
        )
        .map_err(|e| ProxyError::Storage(e.to_string()))?;

        tracing::info!("fixed default admin password (was PLACEHOLDER_BCRYPT_HASH)");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chennix_storage::schema::init_db;
    use std::sync::Mutex;

    /// 串行化需要操作环境变量的测试（cargo test 默认并发运行，
    /// 环境变量是进程级共享状态，并发修改会互相干扰）。
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_config_yaml(yaml: &str) -> std::path::PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "chennix_test_config_{}.yaml",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&tmp, yaml).unwrap();
        tmp
    }

    #[test]
    fn test_load_config_full() {
        // 串行化：环境变量是进程级共享状态，并发测试会互相干扰
        let _guard = ENV_LOCK.lock().unwrap();
        // 临时清除可能影响测试的环境变量（cargo test 运行时 RUST_LOG 可能已设置）
        let env_keys = ["PORT", "HOST", "DB_PATH", "RUST_LOG",
                        "UPSTREAM_TIMEOUT_SECS", "STREAMING_TIMEOUT_SECS"];
        let saved: Vec<(String, Option<String>)> = env_keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for k in &env_keys {
            std::env::remove_var(k);
        }

        let yaml = r#"
server:
  host: "127.0.0.1"
  port: 9090
  tls:
    enabled: true
    cert: "/path/cert.pem"
    key: "/path/key.pem"
log:
  level: "debug"
bootstrap:
  config_file: "bootstrap.yaml"
database:
  path: "test.db"
"#;
        let tmp = write_config_yaml(yaml);
        let config = load_config(tmp.to_str().unwrap()).unwrap();
        assert_eq!(config.server.host, "127.0.0.1");
        assert_eq!(config.server.port, 9090);
        assert!(config.server.tls.enabled);
        assert_eq!(config.server.tls.cert, "/path/cert.pem");
        assert_eq!(config.server.tls.key, "/path/key.pem");
        assert_eq!(config.log.level, "debug");
        assert_eq!(config.bootstrap.config_file, "bootstrap.yaml");
        assert_eq!(config.database.path, "test.db");

        // 恢复环境变量
        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }

    #[test]
    fn test_load_config_defaults() {
        // 串行化：环境变量是进程级共享状态，并发测试会互相干扰
        let _guard = ENV_LOCK.lock().unwrap();
        // 临时清除可能影响测试的环境变量
        let env_keys = ["PORT", "HOST", "DB_PATH", "RUST_LOG",
                        "UPSTREAM_TIMEOUT_SECS", "STREAMING_TIMEOUT_SECS"];
        let saved: Vec<(String, Option<String>)> = env_keys
            .iter()
            .map(|k| (k.to_string(), std::env::var(k).ok()))
            .collect();
        for k in &env_keys {
            std::env::remove_var(k);
        }

        // Minimal config — server.host, server.port, log.level, database.path
        // should all fall back to defaults.
        let yaml = r#"
server:
  host: ""
log:
  level: "info"
bootstrap:
  config_file: "boot.yaml"
"#;
        let tmp = write_config_yaml(yaml);
        let config = load_config(tmp.to_str().unwrap()).unwrap();
        assert_eq!(config.server.host, "");
        assert_eq!(config.server.port, 8080); // default
        assert!(!config.server.tls.enabled); // default
        assert_eq!(config.log.level, "info");
        assert_eq!(config.bootstrap.config_file, "boot.yaml");
        assert_eq!(config.database.path, "/data/chennix.db"); // default

        // 恢复环境变量
        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }

    #[test]
    fn test_load_config_missing_file() {
        let result = load_config("/nonexistent/path/config.yaml");
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Config(_) => {}
            other => panic!("expected Config error, got {:?}", other),
        }
    }

    #[test]
    fn test_load_config_invalid_yaml() {
        let yaml = "server: { host: \"x\"\n  port: :bad";
        let tmp = write_config_yaml(yaml);
        let result = load_config(tmp.to_str().unwrap());
        assert!(result.is_err());
        match result.unwrap_err() {
            ProxyError::Config(_) => {}
            other => panic!("expected Config error, got {:?}", other),
        }
    }

    #[test]
    fn test_ensure_default_admin_creates_admin_when_empty() {
        // 串行化：环境变量是进程级共享状态
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CHENNIX_ADMIN_PASSWORD").ok();
        std::env::set_var("CHENNIX_ADMIN_PASSWORD", "test-password-123");

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // table is empty
        ensure_default_admin(&conn).unwrap();

        let admin_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE username = 'admin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(admin_count, 1);

        let (role, quota, status, group): (i64, i64, i64, String) = conn
            .query_row(
                "SELECT role, quota, status, \"group\" FROM users WHERE username = 'admin'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(role, 100, "admin role must be 100 (root)");
        assert_eq!(quota, 999_999_999, "admin quota must be 999999999");
        assert_eq!(status, 1, "admin status must be enabled");
        assert_eq!(group, "default");

        // password hash must verify against the env var value
        let hash: String = conn
            .query_row(
                "SELECT password_hash FROM users WHERE username = 'admin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            bcrypt::verify("test-password-123", &hash).unwrap(),
            "admin password must verify against CHENNIX_ADMIN_PASSWORD env var"
        );

        // 恢复环境变量
        match prev {
            Some(val) => std::env::set_var("CHENNIX_ADMIN_PASSWORD", val),
            None => std::env::remove_var("CHENNIX_ADMIN_PASSWORD"),
        }
    }

    #[test]
    fn test_ensure_default_admin_skips_when_no_env_var() {
        // 串行化：环境变量是进程级共享状态
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CHENNIX_ADMIN_PASSWORD").ok();
        std::env::remove_var("CHENNIX_ADMIN_PASSWORD");

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // table is empty, no env var → 应跳过创建
        ensure_default_admin(&conn).unwrap();

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 0, "no admin should be created when CHENNIX_ADMIN_PASSWORD not set");

        // 恢复环境变量
        match prev {
            Some(val) => std::env::set_var("CHENNIX_ADMIN_PASSWORD", val),
            None => std::env::remove_var("CHENNIX_ADMIN_PASSWORD"),
        }
    }

    #[test]
    fn test_ensure_default_admin_noop_when_users_exist() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // pre-insert a user
        conn.execute(
            "INSERT INTO users (username, password_hash, role, status, quota, used_quota, \"group\")
             VALUES ('existing', 'hash', 1, 1, 100, 0, 'default')",
            [],
        )
        .unwrap();

        ensure_default_admin(&conn).unwrap();

        // admin must NOT have been created
        let admin_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM users WHERE username = 'admin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(admin_count, 0, "admin must not be created when users exist");

        // total user count must remain 1
        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_ensure_default_admin_idempotent() {
        // 串行化：环境变量是进程级共享状态
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CHENNIX_ADMIN_PASSWORD").ok();
        std::env::set_var("CHENNIX_ADMIN_PASSWORD", "test-pass-idempotent");

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // first call creates admin
        ensure_default_admin(&conn).unwrap();
        let count_after_first: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after_first, 1);

        // second call is a no-op
        ensure_default_admin(&conn).unwrap();
        let count_after_second: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count_after_second, 1, "second call must not duplicate admin");

        // 恢复环境变量
        match prev {
            Some(v) => std::env::set_var("CHENNIX_ADMIN_PASSWORD", v),
            None => std::env::remove_var("CHENNIX_ADMIN_PASSWORD"),
        }
    }

    #[test]
    fn test_ensure_default_admin_skips_when_env_var_empty() {
        // 串行化：环境变量是进程级共享状态
        let _guard = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("CHENNIX_ADMIN_PASSWORD").ok();
        // 设为空字符串：应视为未设置，跳过创建
        std::env::set_var("CHENNIX_ADMIN_PASSWORD", "");

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        ensure_default_admin(&conn).unwrap();

        let total: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            total, 0,
            "no admin should be created when CHENNIX_ADMIN_PASSWORD is empty string"
        );

        // 恢复环境变量
        match prev {
            Some(v) => std::env::set_var("CHENNIX_ADMIN_PASSWORD", v),
            None => std::env::remove_var("CHENNIX_ADMIN_PASSWORD"),
        }
    }
}
