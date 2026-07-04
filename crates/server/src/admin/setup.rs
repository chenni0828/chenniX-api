//! 首次启动初始化向导 API — 公开端点，无需 session。
//!
//! - `GET /admin/api/setup/status` — 返回是否需要初始化
//! - `POST /admin/api/setup/initialize` — 创建首个管理员账号
//!
//! 安全：initialize 仅在 users 表为空时可用。db 锁在整个 handler 期间持有，
//! count 检查与 create 原子串行，无 TOCTOU 竞态。

use axum::extract::State;
use axum::Json;
use serde::{Deserialize, Serialize};

use chennix_common::QUOTA_PER_YUAN;
use chennix_storage::users::UserRepo;

use crate::admin::error::{AdminError, AdminResult};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct SetupStatusResponse {
    pub needs_setup: bool,
}

/// `GET /admin/api/setup/status` — 检查是否需要初始化。
/// users 表为空时 needs_setup=true。
pub async fn setup_status_handler(
    State(state): State<AppState>,
) -> AdminResult<Json<SetupStatusResponse>> {
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);
    let count = repo.count_users()?;
    Ok(Json(SetupStatusResponse { needs_setup: count == 0 }))
}

#[derive(Debug, Deserialize)]
pub struct InitializeRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct InitializeResponse {
    pub success: bool,
}

/// `POST /admin/api/setup/initialize` — 创建首个管理员。
/// 仅当 users 表为空时可用，否则 400（系统已初始化）。
pub async fn setup_initialize_handler(
    State(state): State<AppState>,
    Json(payload): Json<InitializeRequest>,
) -> AdminResult<Json<InitializeResponse>> {
    // 1. 校验输入
    let username = payload.username.trim();
    if username.is_empty() {
        return Err(AdminError::BadRequest("用户名不能为空".into()));
    }
    if payload.password.len() < 8 {
        return Err(AdminError::BadRequest("密码长度至少 8 位".into()));
    }

    // 2. 持有 db 锁：count 检查 + create 原子串行，无竞态
    let db = state.db.lock().await;
    let repo = UserRepo::new(&db);
    let count = repo.count_users()?;
    if count > 0 {
        return Err(AdminError::BadRequest("系统已初始化，请直接登录".into()));
    }

    // 3. 创建管理员（role=100, quota=999_999_999 元 = 999_999_999 微元 × QUOTA_PER_YUAN, group=default, status=1）
    let hash = bcrypt::hash(&payload.password, bcrypt::DEFAULT_COST)
        .map_err(|e| AdminError::Internal(format!("bcrypt hash failed: {}", e)))?;
    repo.create_user_with_quota(username, &hash, 100, "default", 999_999_999_i64 * QUOTA_PER_YUAN)?;

    tracing::info!("setup wizard: created initial admin user '{}'", username);
    Ok(Json(InitializeResponse { success: true }))
}

#[cfg(test)]
mod tests {
    use crate::admin::routes::admin_router;
    use crate::pipeline::StorageAdapter;
    use crate::state::AppState;
    use chennix_core::cache::ConfigCache;
    use chennix_core::executor::Executor;
    use chennix_core::health::HealthManager;
    use chennix_core::normalizer::Normalizer;
    use chennix_storage::schema::init_db;
    use rusqlite::Connection;
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    /// 构建空库 AppState（无用户，needs_setup=true）。
    async fn empty_state() -> AppState {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        build_state(conn)
    }

    /// 构建含一个普通用户的 AppState（needs_setup=false）。
    async fn initialized_state() -> AppState {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash, role, status, quota, used_quota, \"group\")
             VALUES ('someone', 'hash', 1, 1, 100, 0, 'default')",
            [],
        )
        .unwrap();
        build_state(conn)
    }

    fn build_state(conn: Connection) -> AppState {
        let db = Arc::new(Mutex::new(conn));
        let storage = Arc::new(StorageAdapter::new(db.clone()));
        let normalizer = Arc::new(Normalizer::new());
        let cache = Arc::new(ConfigCache::new(normalizer.clone()));
        let health = Arc::new(HealthManager::with_db(db.clone()));
        let http_client = reqwest::Client::new();
        let executor = Arc::new(Executor::new(
            health.clone(),
            cache.clone(),
            http_client.clone(),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(300),
        ));
        AppState {
            executor,
            cache,
            health,
            normalizer,
            storage,
            db,
            config: Arc::new(crate::config::AppConfig {
                server: crate::config::ServerConfig {
                    host: "127.0.0.1".into(),
                    port: 8080,
                    tls: crate::config::TlsConfig::default(),
                    upstream_timeout_secs: 60,
                    streaming_timeout_secs: 300,
                },
                log: crate::config::LogConfig::default(),
                bootstrap: crate::config::BootstrapConfig {
                    config_file: String::new(),
                },
                database: crate::config::DatabaseConfig::default(),
            }),
            session_store: crate::admin::auth::new_session_store(),
            active_streams: Arc::new(AtomicUsize::new(0)),
            http_client,
        }
    }

    #[tokio::test]
    async fn test_setup_status_needs_setup_when_empty() {
        let state = empty_state().await;
        let app = admin_router(state.clone()).with_state(state);
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("GET")
                .uri("/admin/api/setup/status")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["needs_setup"], true);
    }

    #[tokio::test]
    async fn test_setup_status_no_setup_when_users_exist() {
        let state = initialized_state().await;
        let app = admin_router(state.clone()).with_state(state);
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("GET")
                .uri("/admin/api/setup/status")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), 200);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["needs_setup"], false);
    }

    #[tokio::test]
    async fn test_setup_initialize_creates_admin() {
        let state = empty_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());
        let req_body =
            serde_json::json!({ "username": "rootadmin", "password": "strong-pass-123" });
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/setup/initialize")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), 200);
        // 验证用户已创建
        let db = state.db.lock().await;
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM users WHERE username='rootadmin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let role: i64 = db
            .query_row(
                "SELECT role FROM users WHERE username='rootadmin'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(role, 100);
    }

    #[tokio::test]
    async fn test_setup_initialize_rejects_when_users_exist() {
        let state = initialized_state().await;
        let app = admin_router(state.clone()).with_state(state);
        let req_body =
            serde_json::json!({ "username": "newadmin", "password": "strong-pass-123" });
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/setup/initialize")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), 400);
    }

    #[tokio::test]
    async fn test_setup_initialize_rejects_short_password() {
        let state = empty_state().await;
        let app = admin_router(state.clone()).with_state(state);
        let req_body = serde_json::json!({ "username": "admin", "password": "short" });
        let resp = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/setup/initialize")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(resp.status(), 400);
    }
}
