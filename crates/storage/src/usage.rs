use chennix_common::{DashboardOverview, ModelUsage, ProxyError, ProxyResult, RequestLog, TokenUsageStats, Usage, UsageSummary};
use rusqlite::{params, Connection, OptionalExtension};

pub struct UsageRepo<'a> {
    conn: &'a Connection,
}

impl<'a> UsageRepo<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn log_usage(
        &self,
        channel_id: i64,
        key_id: i64,
        model_id: i64,
        usage: &Usage,
        request_type: &str,
        status: &str,
        error_message: Option<&str>,
        user_id: i64,
        token_id: i64,
        quota_cost: i64,
    ) -> ProxyResult<i64> {
        self.conn
            .execute(
                "INSERT INTO usage_logs
                 (channel_id, key_id, model_id, user_id, token_id, quota_cost,
                  prompt_tokens, completion_tokens, total_tokens,
                  request_type, status, error_message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    channel_id,
                    key_id,
                    model_id,
                    user_id,
                    token_id,
                    quota_cost,
                    usage.prompt_tokens,
                    usage.completion_tokens,
                    usage.total_tokens,
                    request_type,
                    status,
                    error_message,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn log_request(
        &self,
        request_id: &str,
        client_ip: Option<&str>,
        method: &str,
        path: &str,
        client_model: Option<&str>,
        normalized_model: Option<&str>,
        channel_name: Option<&str>,
        key_label: Option<&str>,
        attempted_keys: Option<&str>,
        upstream_status: Option<i64>,
        response_status: i64,
        duration_ms: i64,
        stream: bool,
        error_message: Option<&str>,
        user_id: Option<i64>,
        token_id: Option<i64>,
        quota_cost: i64,
    ) -> ProxyResult<i64> {
        self.conn
            .execute(
                "INSERT INTO request_logs
                 (request_id, client_ip, method, path, client_model, normalized_model,
                  channel_name, key_label, attempted_keys, upstream_status, response_status,
                  duration_ms, stream, user_id, token_id, quota_cost, error_message)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
                params![
                    request_id,
                    client_ip,
                    method,
                    path,
                    client_model,
                    normalized_model,
                    channel_name,
                    key_label,
                    attempted_keys,
                    upstream_status,
                    response_status,
                    duration_ms,
                    if stream { 1 } else { 0 },
                    user_id,
                    token_id,
                    quota_cost,
                    error_message,
                ],
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Daily aggregate usage series across all users (admin view).
    /// Returns `(day_string, total_tokens, total_quota_cost)` per day.
    pub fn get_all_usage(&self, days: u32) -> ProxyResult<Vec<(String, u64, i64)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DATE(created_at) as day,
                        SUM(total_tokens) as tokens,
                        SUM(quota_cost) as quota
                 FROM usage_logs
                 WHERE created_at >= datetime('now', ?1)
                 GROUP BY day ORDER BY day",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let days_str = format!("-{} days", days);
        let rows = stmt
            .query_map(params![days_str], |r| {
                let tokens: i64 = r.get(1)?;
                let quota: i64 = r.get(2)?;
                Ok((r.get::<_, String>(0)?, tokens as u64, quota))
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// Daily aggregate usage series filtered to a single user.
    /// Returns `(day_string, total_tokens, total_quota_cost)` per day.
    pub fn get_user_usage(
        &self,
        user_id: i64,
        days: u32,
    ) -> ProxyResult<Vec<(String, u64, i64)>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT DATE(created_at) as day,
                        SUM(total_tokens) as tokens,
                        SUM(quota_cost) as quota
                 FROM usage_logs
                 WHERE user_id = ?1 AND created_at >= datetime('now', ?2)
                 GROUP BY day ORDER BY day",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let days_str = format!("-{} days", days);
        let rows = stmt
            .query_map(params![user_id, days_str], |r| {
                let tokens: i64 = r.get(1)?;
                let quota: i64 = r.get(2)?;
                Ok((r.get::<_, String>(0)?, tokens as u64, quota))
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// Backwards-compatible daily tokens series (no quota_cost) for callers
    /// that only need token counts.
    pub fn get_daily_usage_series(&self, days: u32) -> ProxyResult<Vec<(String, u64)>> {
        let all = self.get_all_usage(days)?;
        Ok(all.into_iter().map(|(d, t, _)| (d, t)).collect())
    }

    /// Paginated request log entries for a single user.
    ///
    /// Columns returned:
    /// `(id, request_id, method, path, client_model, normalized_model,
    ///   channel_name, response_status, duration_ms, stream, quota_cost,
    ///   created_at, error_message)`
    pub fn get_user_request_logs(
        &self,
        user_id: i64,
        limit: u32,
        offset: u32,
    ) -> ProxyResult<Vec<RequestLogRow>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, request_id, method, path, client_model, normalized_model,
                        channel_name, response_status, duration_ms, stream, quota_cost,
                        created_at, error_message
                 FROM request_logs
                 WHERE user_id = ?1
                 ORDER BY id DESC
                 LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![user_id, limit, offset], map_request_log_row)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    // ===== Admin API methods =====

    /// Dashboard overview: today's tokens, requests, errors, and available keys.
    ///
    /// Runs four sub-queries in a single SQL statement for efficiency.
    pub fn get_dashboard_overview(&self) -> ProxyResult<DashboardOverview> {
        let row = self
            .conn
            .query_row(
                "SELECT
                   (SELECT COALESCE(SUM(total_tokens), 0) FROM usage_logs
                    WHERE DATE(created_at) = DATE('now')),
                   (SELECT COUNT(*) FROM request_logs
                    WHERE DATE(created_at) = DATE('now')),
                   (SELECT COUNT(*) FROM request_logs
                    WHERE DATE(created_at) = DATE('now') AND response_status >= 400),
                   (SELECT COUNT(*) FROM channel_keys WHERE status = 'active')",
                [],
                |r| {
                    Ok(DashboardOverview {
                        today_tokens: r.get(0)?,
                        today_requests: r.get(1)?,
                        today_errors: r.get(2)?,
                        available_keys: r.get(3)?,
                    })
                },
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        Ok(row)
    }

    /// Top N models by total token consumption today.
    ///
    /// Joins `usage_logs` with `models` to resolve the canonical name.
    pub fn get_top_models(&self, limit: i64) -> ProxyResult<Vec<ModelUsage>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT m.canonical_name,
                        COALESCE(SUM(u.total_tokens), 0),
                        COUNT(*),
                        COALESCE(SUM(u.quota_cost), 0)
                 FROM usage_logs u
                 JOIN models m ON u.model_id = m.id
                 WHERE DATE(u.created_at) = DATE('now')
                 GROUP BY m.canonical_name
                 ORDER BY SUM(u.total_tokens) DESC
                 LIMIT ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![limit], |r| {
                Ok(ModelUsage {
                    model: r.get(0)?,
                    total_tokens: r.get(1)?,
                    request_count: r.get(2)?,
                    total_cost: r.get(3)?,
                })
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// Recent N request log entries (newest first).
    pub fn get_recent_requests(&self, limit: i64) -> ProxyResult<Vec<RequestLog>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, request_id, client_ip, method, path, client_model,
                        normalized_model, channel_name, key_label, upstream_status,
                        response_status, duration_ms, stream, user_id, token_id,
                        quota_cost, error_message, created_at
                 FROM request_logs
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params![limit], map_admin_request_log)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// Aggregated usage summary grouped by channel + model within a time range.
    ///
    /// `start` / `end` are Unix timestamps (seconds). Pass `0` for either to
    /// skip that bound. `channel_id`, `model`, and `user_id` are optional filters.
    /// When `user_id` is `Some(uid)`, only that user's usage is included.
    pub fn get_usage_summary(
        &self,
        channel_id: Option<i64>,
        model: Option<&str>,
        start: i64,
        end: i64,
        user_id: Option<i64>,
    ) -> ProxyResult<Vec<UsageSummary>> {
        let mut sql = String::from(
            "SELECT u.channel_id, c.name, m.canonical_name,
                    COALESCE(SUM(u.total_tokens), 0),
                    COUNT(*),
                    COALESCE(SUM(u.quota_cost), 0)
             FROM usage_logs u
             JOIN channels c ON u.channel_id = c.id
             JOIN models m ON u.model_id = m.id
             WHERE 1=1",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(uid) = user_id {
            sql.push_str(&format!(" AND u.user_id = ?{}", param_idx));
            params_vec.push(Box::new(uid));
            param_idx += 1;
        }
        if start > 0 {
            sql.push_str(&format!(" AND u.created_at >= datetime(?{}, 'unixepoch')", param_idx));
            params_vec.push(Box::new(start));
            param_idx += 1;
        }
        if end > 0 {
            sql.push_str(&format!(" AND u.created_at <= datetime(?{}, 'unixepoch')", param_idx));
            params_vec.push(Box::new(end));
            param_idx += 1;
        }
        if let Some(cid) = channel_id {
            sql.push_str(&format!(" AND u.channel_id = ?{}", param_idx));
            params_vec.push(Box::new(cid));
            param_idx += 1;
        }
        if let Some(m) = model {
            sql.push_str(&format!(" AND m.canonical_name = ?{}", param_idx));
            params_vec.push(Box::new(m.to_string()));
        }
        sql.push_str(" GROUP BY u.channel_id, m.canonical_name ORDER BY SUM(u.total_tokens) DESC");

        let mut stmt = self
            .conn
            .prepare(&sql)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec.iter()), |r| {
                Ok(UsageSummary {
                    channel_id: r.get(0)?,
                    channel_name: r.get(1)?,
                    model: r.get(2)?,
                    total_tokens: r.get(3)?,
                    request_count: r.get(4)?,
                    total_cost: r.get(5)?,
                })
            })
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok(result)
    }

    /// Paginated request logs with optional filters.
    ///
    /// Returns `(logs, total_count)`. `page` is 1-based.
    /// `channel_id` filters by joining `channels.id` → `request_logs.channel_name`.
    /// `model` filters by `normalized_model`.
    /// `status_code` filters by `response_status`.
    /// `user_id` filters by the requesting user.
    /// `start` / `end` are Unix timestamps (0 = unbounded).
    pub fn get_request_logs(
        &self,
        page: i64,
        per_page: i64,
        channel_id: Option<i64>,
        model: Option<&str>,
        status_code: Option<i32>,
        start: i64,
        end: i64,
        user_id: Option<i64>,
    ) -> ProxyResult<(Vec<RequestLog>, i64)> {
        let mut where_clause = String::from("WHERE 1=1");
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();
        let mut idx = 1;

        if let Some(uid) = user_id {
            where_clause.push_str(&format!(" AND user_id = ?{}", idx));
            params_vec.push(Box::new(uid));
            idx += 1;
        }
        if start > 0 {
            where_clause.push_str(&format!(" AND created_at >= datetime(?{}, 'unixepoch')", idx));
            params_vec.push(Box::new(start));
            idx += 1;
        }
        if end > 0 {
            where_clause.push_str(&format!(" AND created_at <= datetime(?{}, 'unixepoch')", idx));
            params_vec.push(Box::new(end));
            idx += 1;
        }
        if let Some(cid) = channel_id {
            where_clause.push_str(&format!(" AND channel_name = (SELECT name FROM channels WHERE id = ?{})", idx));
            params_vec.push(Box::new(cid));
            idx += 1;
        }
        if let Some(m) = model {
            where_clause.push_str(&format!(" AND normalized_model = ?{}", idx));
            params_vec.push(Box::new(m.to_string()));
            idx += 1;
        }
        if let Some(sc) = status_code {
            where_clause.push_str(&format!(" AND response_status = ?{}", idx));
            params_vec.push(Box::new(sc));
            idx += 1;
        }

        // Count total
        let count_sql = format!("SELECT COUNT(*) FROM request_logs {}", where_clause);
        let total: i64 = self
            .conn
            .query_row(&count_sql, rusqlite::params_from_iter(params_vec.iter()), |r| r.get(0))
            .map_err(|e| ProxyError::Storage(e.to_string()))?;

        // Query page
        let offset = (page - 1) * per_page;
        let query_sql = format!(
            "SELECT id, request_id, client_ip, method, path, client_model,
                    normalized_model, channel_name, key_label, upstream_status,
                    response_status, duration_ms, stream, user_id, token_id,
                    quota_cost, error_message, created_at
             FROM request_logs
             {}
             ORDER BY id DESC
             LIMIT ?{} OFFSET ?{}",
            where_clause,
            idx,
            idx + 1
        );
        params_vec.push(Box::new(per_page));
        params_vec.push(Box::new(offset));

        let mut stmt = self
            .conn
            .prepare(&query_sql)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(rusqlite::params_from_iter(params_vec.iter()), map_admin_request_log)
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let mut result = Vec::new();
        for row in rows {
            result.push(row.map_err(|e| ProxyError::Storage(e.to_string()))?);
        }
        Ok((result, total))
    }

    /// Per-token usage statistics: total tokens consumed, request count, and last used time.
    pub fn get_token_usage_stats(&self, token_id: i64) -> ProxyResult<TokenUsageStats> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT COALESCE(SUM(total_tokens), 0), COUNT(*),
                        MAX(created_at)
                 FROM usage_logs WHERE token_id = ?1",
            )
            .map_err(|e| ProxyError::Storage(e.to_string()))?;
        let stats: TokenUsageStats = stmt
            .query_row(params![token_id], |r| {
                let total_tokens: i64 = r.get(0)?;
                let request_count: i64 = r.get(1)?;
                let last_used_at: Option<String> = r.get(2)?;
                Ok(TokenUsageStats {
                    total_tokens,
                    request_count,
                    last_used_at,
                })
            })
            .optional()
            .map_err(|e| ProxyError::Storage(e.to_string()))?
            .unwrap_or(TokenUsageStats {
                total_tokens: 0,
                request_count: 0,
                last_used_at: None,
            });
        Ok(stats)
    }
}

/// Map a `request_logs` row to the `RequestLog` type from `chennix_common`.
fn map_admin_request_log(r: &rusqlite::Row<'_>) -> rusqlite::Result<RequestLog> {
    let stream: i64 = r.get(12)?;
    Ok(RequestLog {
        id: r.get(0)?,
        request_id: r.get(1)?,
        client_ip: r.get(2)?,
        method: r.get(3)?,
        path: r.get(4)?,
        client_model: r.get(5)?,
        normalized_model: r.get(6)?,
        channel_name: r.get(7)?,
        key_label: r.get(8)?,
        upstream_status: r.get::<_, Option<i64>>(9)?.map(|v| v as i32),
        response_status: r.get::<_, i64>(10)? as i32,
        duration_ms: r.get(11)?,
        stream: stream != 0,
        user_id: r.get(13)?,
        token_id: r.get(14)?,
        quota_cost: r.get(15)?,
        error_message: r.get(16)?,
        created_at: r.get(17)?,
    })
}

#[derive(Debug, Clone)]
pub struct RequestLogRow {
    pub id: i64,
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub client_model: Option<String>,
    pub normalized_model: Option<String>,
    pub channel_name: Option<String>,
    pub response_status: i64,
    pub duration_ms: i64,
    pub stream: bool,
    pub quota_cost: i64,
    pub created_at: String,
    pub error_message: Option<String>,
}

fn map_request_log_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<RequestLogRow> {
    let stream: i64 = r.get(9)?;
    Ok(RequestLogRow {
        id: r.get(0)?,
        request_id: r.get(1)?,
        method: r.get(2)?,
        path: r.get(3)?,
        client_model: r.get(4)?,
        normalized_model: r.get(5)?,
        channel_name: r.get(6)?,
        response_status: r.get(7)?,
        duration_ms: r.get(8)?,
        stream: stream != 0,
        quota_cost: r.get(10)?,
        created_at: r.get(11)?,
        error_message: r.get(12)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::ChannelRepo;
    use crate::keys::KeyRepo;
    use crate::models::ModelRepo;
    use crate::schema::init_db;
    use crate::users::UserRepo;
    use crate::tokens::TokenRepo;
    use chennix_common::{ChannelProvider, CostTier};

    fn setup() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let ch = ChannelRepo::new(&conn);
        ch.create_channel("test", &ChannelProvider::OpenaiCompatible, "http://test").unwrap();
        let kr = KeyRepo::new(&conn);
        kr.create_key(1, "sk", None, CostTier::Paid, 1, None, None, None).unwrap();
        let mr = ModelRepo::new(&conn);
        mr.create_model("test-model").unwrap();
        let ur = UserRepo::new(&conn);
        ur.create_user("alice", "hash", 1, "default").unwrap();
        let tr = TokenRepo::new(&conn);
        tr.create_token(1, "sk-user-token", Some("alice-tok"), 1000, false).unwrap();
        conn
    }

    #[test]
    fn test_log_usage_with_billing() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);
        let usage = Usage { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150 };
        let id = repo
            .log_usage(1, 1, 1, &usage, "chat", "success", None, 1, 1, 30)
            .unwrap();
        assert!(id > 0);

        let series = repo.get_daily_usage_series(7).unwrap();
        assert!(!series.is_empty());
        assert!(series[0].1 >= 150);

        // user-scoped query
        let user_series = repo.get_user_usage(1, 7).unwrap();
        assert!(!user_series.is_empty());
        assert_eq!(user_series[0].1, 150);
        assert_eq!(user_series[0].2, 30);

        // admin (all users) query — same data here
        let all = repo.get_all_usage(7).unwrap();
        assert!(!all.is_empty());
        assert_eq!(all[0].1, 150);
        assert_eq!(all[0].2, 30);
    }

    #[test]
    fn test_log_usage_filters_by_user() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);
        let ur = UserRepo::new(&conn);
        ur.create_user("bob", "hash", 1, "default").unwrap();
        // bob has id=2

        let usage = Usage { prompt_tokens: 50, completion_tokens: 25, total_tokens: 75 };
        // alice usage
        repo.log_usage(1, 1, 1, &usage, "chat", "success", None, 1, 1, 10).unwrap();
        // bob usage
        repo.log_usage(1, 1, 1, &usage, "chat", "success", None, 2, 1, 20).unwrap();

        // alice sees only her row
        let alice = repo.get_user_usage(1, 7).unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].1, 75);
        assert_eq!(alice[0].2, 10);

        // bob sees only his row
        let bob = repo.get_user_usage(2, 7).unwrap();
        assert_eq!(bob.len(), 1);
        assert_eq!(bob[0].2, 20);

        // admin sees aggregate
        let all = repo.get_all_usage(7).unwrap();
        assert_eq!(all.len(), 1); // same day
        assert_eq!(all[0].1, 150); // 75 + 75
        assert_eq!(all[0].2, 30);  // 10 + 20
    }

    #[test]
    fn test_log_usage_zero_quota_cost_default() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);
        let usage = Usage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 };
        repo.log_usage(1, 1, 1, &usage, "chat", "success", None, 1, 1, 0).unwrap();

        let all = repo.get_all_usage(7).unwrap();
        assert_eq!(all[0].2, 0);
    }

    #[test]
    fn test_log_request_with_billing() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);
        let id = repo
            .log_request(
                "req-123", Some("127.0.0.1"), "POST", "/v1/chat/completions",
                Some("deepseek-v3"), Some("deepseek-v3"), Some("test"), Some("sk"),
                Some(r#"["sk"]"#), Some(200), 200, 150, false, None,
                Some(1), Some(1), 25,
            )
            .unwrap();
        assert!(id > 0);

        // user-scoped request log query
        let logs = repo.get_user_request_logs(1, 10, 0).unwrap();
        assert_eq!(logs.len(), 1);
        let row = &logs[0];
        assert_eq!(row.request_id, "req-123");
        assert_eq!(row.method, "POST");
        assert_eq!(row.path, "/v1/chat/completions");
        assert_eq!(row.response_status, 200);
        assert_eq!(row.duration_ms, 150);
        assert!(!row.stream);
        assert_eq!(row.quota_cost, 25);
        assert_eq!(row.client_model.as_deref(), Some("deepseek-v3"));
        assert_eq!(row.channel_name.as_deref(), Some("test"));
        assert!(row.error_message.is_none());
    }

    #[test]
    fn test_get_user_request_logs_pagination_and_isolation() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);
        let ur = UserRepo::new(&conn);
        ur.create_user("bob", "hash", 1, "default").unwrap(); // bob id=2

        // insert 5 logs for alice, 2 for bob
        for i in 0..5 {
            repo.log_request(
                &format!("alice-{}", i), None, "POST", "/v1/chat", None, None,
                None, None, None, Some(200), 200, 10 * i, false, None,
                Some(1), Some(1), i,
            )
            .unwrap();
        }
        for i in 0..2 {
            repo.log_request(
                &format!("bob-{}", i), None, "POST", "/v1/chat", None, None,
                None, None, None, Some(200), 200, 10, false, None,
                Some(2), Some(1), i,
            )
            .unwrap();
        }

        // alice has 5 total
        let all_alice = repo.get_user_request_logs(1, 100, 0).unwrap();
        assert_eq!(all_alice.len(), 5);

        // pagination: first 2 (DESC order → newest first)
        let page1 = repo.get_user_request_logs(1, 2, 0).unwrap();
        assert_eq!(page1.len(), 2);
        // newest entries have higher ids → "alice-4" first
        assert_eq!(page1[0].request_id, "alice-4");
        assert_eq!(page1[1].request_id, "alice-3");

        let page2 = repo.get_user_request_logs(1, 2, 2).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].request_id, "alice-2");

        let page3 = repo.get_user_request_logs(1, 2, 4).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].request_id, "alice-0");

        // bob isolation: only his 2 logs
        let bob_logs = repo.get_user_request_logs(2, 100, 0).unwrap();
        assert_eq!(bob_logs.len(), 2);
        for log in &bob_logs {
            assert!(log.request_id.starts_with("bob-"));
        }
    }

    #[test]
    fn test_get_user_usage_no_rows() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);
        // user 999 has no rows
        let result = repo.get_user_usage(999, 7).unwrap();
        assert!(result.is_empty());

        let logs = repo.get_user_request_logs(999, 10, 0).unwrap();
        assert!(logs.is_empty());
    }

    #[test]
    fn test_log_request_with_null_user_token_anonymous() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);
        // anonymous request (no user / token)
        repo.log_request(
            "req-anon", Some("127.0.0.1"), "GET", "/health", None, None,
            None, None, None, None, 200, 5, false, None,
            None, None, 0,
        )
        .unwrap();
        // user 1 should NOT see anonymous logs
        let logs = repo.get_user_request_logs(1, 10, 0).unwrap();
        assert_eq!(logs.len(), 0);
    }

    #[test]
    fn test_get_token_usage_stats() {
        let conn = setup();
        let repo = UsageRepo::new(&conn);

        // No usage yet for token 1
        let stats = repo.get_token_usage_stats(1).unwrap();
        assert_eq!(stats.total_tokens, 0);
        assert_eq!(stats.request_count, 0);
        assert!(stats.last_used_at.is_none());

        // Log some usage for token_id=1
        let usage = Usage { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150 };
        repo.log_usage(1, 1, 1, &usage, "chat", "success", None, 1, 1, 30).unwrap();
        repo.log_usage(1, 1, 1, &usage, "chat", "success", None, 1, 1, 20).unwrap();

        let stats = repo.get_token_usage_stats(1).unwrap();
        assert_eq!(stats.total_tokens, 300); // 150 * 2
        assert_eq!(stats.request_count, 2);
        assert!(stats.last_used_at.is_some());
    }
}
