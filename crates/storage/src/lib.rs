pub mod bootstrap;
pub mod channels;
pub mod keys;
pub mod models;
pub mod schema;
pub mod tokens;
pub mod usage;
pub mod users;

use chennix_common::ProxyResult;
use rusqlite::Connection;

pub fn open_db(path: &str) -> ProxyResult<Connection> {
    let conn = Connection::open(path)
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
        .map_err(|e| chennix_common::ProxyError::Storage(e.to_string()))?;
    schema::init_db(&conn)?;
    Ok(conn)
}

/// 生成当前时间的 RFC 3339 字符串，时区固定为北京时间（UTC+08:00）。
///
/// 项目面向中国用户，所有时间字段统一存为带 `+08:00` 偏移的 ISO 8601
/// 文本（如 `2026-07-05T01:51:00+08:00`）。这样：
/// - 前端 `new Date(ts)` 原生解析，无需手动补 `Z` 或时区修饰
/// - SQLite `DATE(created_at, '+8 hours')` 取北京日期，仪表盘「今日」正确
///
/// 使用 `FixedOffset` 而非 `Local`，避免依赖服务器时区设置——无论部署
/// 在哪个时区的容器中，写入的都是北京时间。
pub fn now_iso8601() -> String {
    use chrono::{FixedOffset, Utc};
    let tz = FixedOffset::east_opt(8 * 3600).expect("UTC+8 is always valid");
    Utc::now().with_timezone(&tz).to_rfc3339()
}
