//! Admin session authentication: SessionStore, login/logout/me handlers,
//! and session middleware.
//!
//! ## Authentication flow
//! 1. Client `POST /admin/api/auth/login { username, password }`.
//! 2. Server verifies bcrypt hash, creates a UUID session token, stores
//!    `token → user_id` in `SessionStore`.
//! 3. Server returns `Set-Cookie: chennix_session=<token>; Path=/; HttpOnly`.
//! 4. Subsequent requests carry the cookie automatically.
//! 5. `session_middleware` reads the cookie, looks up `SessionStore`, loads
//!    `UserConfig`, and injects `AdminAuthContext` into request extensions.
//! 6. Handlers extract `AdminAuthContext` via `Extension<AdminAuthContext>`.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::RwLock;
use tracing::debug;

use chennix_common::{AdminAuthContext, UserConfig};
use chennix_storage::users::UserRepo;

use crate::admin::error::{AdminError, AdminResult};
use crate::state::AppState;

/// In-memory session store: `session_token → (user_id, created_at)`.
///
/// `created_at` is a Unix timestamp (seconds) recorded when the session was
/// created.  Sessions older than 24 hours are lazily evicted during
/// validation to prevent unbounded memory growth.
///
/// Wrapped in `Arc<RwLock<...>>` for cheap-cloned, concurrent access.
/// Sessions are lost on server restart — this is intentional for the MVP;
/// users simply re-login.
pub type SessionStore = Arc<RwLock<HashMap<String, (i64, i64)>>>;

/// Session lifetime in seconds (24 hours).  Must match the cookie `Max-Age`.
const SESSION_MAX_AGE_SECS: i64 = 86_400;

/// Create a fresh, empty `SessionStore`.
pub fn new_session_store() -> SessionStore {
    Arc::new(RwLock::new(HashMap::new()))
}

// ===== Request / response DTOs =====

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub success: bool,
    pub user: UserConfig,
}

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub user: UserConfig,
}

// ===== Handlers =====

/// `POST /admin/api/auth/login` — verify credentials and create a session.
///
/// On success, sets a `chennix_session` cookie and returns the user object.
pub async fn login_handler(
    State(state): State<AppState>,
    Json(payload): Json<LoginRequest>,
) -> AdminResult<Response> {
    // 1-2. Look up the user and verify password.
    //    Scoped so the db MutexGuard (which is !Send because rusqlite::Connection
    //    is !Sync) is dropped before we await on session_store, keeping the
    //    handler future Send.
    let user = {
        let db = state.db.lock().await;
        let repo = UserRepo::new(&db);

        let user = repo
            .get_user_by_username(&payload.username)
            .map_err(AdminError::from)?;

        let user = match user {
            Some(u) if u.status == 1 => u,
            Some(_) => return Err(AdminError::Unauthorized("account is disabled".into())),
            None => return Err(AdminError::Unauthorized("invalid credentials".into())),
        };

        let hash = repo
            .get_password_hash(&payload.username)
            .map_err(AdminError::from)?
            .ok_or_else(|| AdminError::Internal("password hash missing".into()))?;

        let valid = bcrypt::verify(&payload.password, &hash).unwrap_or(false);
        if !valid {
            debug!("admin login failed for username={}", payload.username);
            return Err(AdminError::Unauthorized("invalid credentials".into()));
        }

        user
    }; // db lock dropped here

    // 3. Create session token and store it.
    let token = uuid::Uuid::new_v4().to_string();
    let now = current_timestamp();
    state
        .session_store
        .write()
        .await
        .insert(token.clone(), (user.id, now));

    tracing::info!("admin login success: username={} user_id={}", user.username, user.id);

    // 4. Build response with Set-Cookie header.
    //    Add `Secure` flag only when TLS is enabled.
    let mut response = Json(LoginResponse {
        success: true,
        user: user.clone(),
    })
    .into_response();

    let secure = if state.config.server.tls.enabled { "; Secure" } else { "" };
    let cookie_value = format!(
        "chennix_session={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400{}",
        token, secure
    );
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_str(&cookie_value)
            .map_err(|e| AdminError::Internal(format!("invalid cookie header: {}", e)))?,
    );

    Ok(response)
}

/// `POST /admin/api/auth/logout` — clear the current session.
///
/// Reads the `chennix_session` cookie, removes it from the `SessionStore`,
/// and returns a cookie that expires immediately.
pub async fn logout_handler(
    State(state): State<AppState>,
    req: Request<Body>,
) -> Response {
    if let Some(token) = extract_session_token(&req) {
        state.session_store.write().await.remove(&token);
    }

    // Set cookie to expire immediately.
    let mut response = Json(json!({ "success": true })).into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        header::HeaderValue::from_static(
            "chennix_session=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
        ),
    );
    response
}

/// `GET /admin/api/auth/me` — return the currently logged-in user.
///
/// Requires `session_middleware` to have injected `AdminAuthContext`.
pub async fn me_handler(
    axum::Extension(auth): axum::Extension<AdminAuthContext>,
) -> AdminResult<Json<MeResponse>> {
    Ok(Json(MeResponse {
        user: auth.user,
    }))
}

// ===== Session middleware =====

/// Session authentication middleware for admin routes.
///
/// Reads the `chennix_session` cookie, looks up the session token in
/// `SessionStore`, loads the `UserConfig` from the database, and injects
/// `AdminAuthContext` into request extensions.
///
/// If no valid session is found, returns **401 Unauthorized**.
pub async fn session_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let token = match extract_session_token(&req) {
        Some(t) => t,
        None => return session_error("not authenticated"),
    };

    // Look up the session (read lock only for the common case).
    let user_id = {
        let sessions = state.session_store.read().await;
        let now = current_timestamp();
        match sessions.get(&token).copied() {
            Some((uid, created_at)) if now - created_at <= SESSION_MAX_AGE_SECS => Some(uid),
            _ => None,
        }
    };

    let user_id = match user_id {
        Some(uid) => {
            // Lazy sweep: only acquire write lock if there are expired sessions.
            let needs_sweep = {
                let sessions = state.session_store.read().await;
                let now = current_timestamp();
                sessions
                    .iter()
                    .any(|(_, (_, created_at))| now - created_at > SESSION_MAX_AGE_SECS)
            };
            if needs_sweep {
                let mut sessions = state.session_store.write().await;
                let now = current_timestamp();
                let expired: Vec<String> = sessions
                    .iter()
                    .filter(|(_, (_, created_at))| now - created_at > SESSION_MAX_AGE_SECS)
                    .take(64)
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in expired {
                    sessions.remove(&k);
                }
            }
            uid
        }
        None => {
            // Session not found or expired — clean up under write lock.
            let mut sessions = state.session_store.write().await;
            let now = current_timestamp();
            sessions.remove(&token);
            let expired: Vec<String> = sessions
                .iter()
                .filter(|(_, (_, created_at))| now - created_at > SESSION_MAX_AGE_SECS)
                .take(64)
                .map(|(k, _)| k.clone())
                .collect();
            for k in expired {
                sessions.remove(&k);
            }
            return session_error("invalid or expired session");
        }
    };

    let user = {
        let db = state.db.lock().await;
        let repo = UserRepo::new(&db);
        match repo.get_user_by_id(user_id) {
            Ok(Some(u)) if u.status == 1 => u,
            Ok(Some(_)) => return session_error("account is disabled"),
            Ok(None) => return session_error("user not found"),
            Err(e) => {
                tracing::error!("session middleware storage error: {}", e);
                return session_error("internal error");
            }
        }
    };

    req.extensions_mut().insert(AdminAuthContext { user });
    next.run(req).await
}

// ===== Helpers =====

/// Extract the `chennix_session` value from the `Cookie` header.
fn extract_session_token(req: &Request<Body>) -> Option<String> {
    let cookie_header = req.headers().get(header::COOKIE)?;
    let s = cookie_header.to_str().ok()?;
    for pair in s.split(';') {
        let pair = pair.trim();
        if let Some(val) = pair.strip_prefix("chennix_session=") {
            let val = val.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }
    None
}

/// Return the current Unix timestamp in seconds.
fn current_timestamp() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before UNIX epoch")
        .as_secs() as i64
}

/// Return a 401 JSON error response for unauthenticated admin requests.
fn session_error(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": msg, "code": 401 })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
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
    use tokio::sync::Mutex;

    /// Build a minimal AppState with an in-memory SQLite DB containing
    /// the default admin user (username=admin, password=admin123).
    async fn setup_state() -> AppState {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Create admin user with bcrypt-hashed password.
        let hash = bcrypt::hash("admin123", bcrypt::DEFAULT_COST).unwrap();
        conn.execute(
            "INSERT INTO users (username, password_hash, role, status, quota, used_quota, \"group\")
             VALUES ('admin', ?1, 100, 1, 999999999, 0, 'default')",
            rusqlite::params![hash],
        )
        .unwrap();

        let db: AppState_SharedDb = Arc::new(Mutex::new(conn));
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
            session_store: new_session_store(),
            active_streams: Arc::new(AtomicUsize::new(0)),
            http_client,
        }
    }

    // Use a type alias to keep lines short in the test helper.
    type AppState_SharedDb = crate::state::SharedDb;

    #[tokio::test]
    async fn test_login_success() {
        let state = setup_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());

        let req_body = serde_json::json!({
            "username": "admin",
            "password": "admin123",
        });

        let response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        // Set-Cookie header must be present.
        assert!(response.headers().get(header::SET_COOKIE).is_some());

        let body = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["success"], true);
        assert_eq!(json["user"]["username"], "admin");
    }

    #[tokio::test]
    async fn test_login_wrong_password() {
        let state = setup_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());

        let req_body = serde_json::json!({
            "username": "admin",
            "password": "wrong",
        });

        let response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_login_unknown_user() {
        let state = setup_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());

        let req_body = serde_json::json!({
            "username": "nobody",
            "password": "whatever",
        });

        let response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_me_without_session_returns_401() {
        let state = setup_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());

        let response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("GET")
                .uri("/admin/api/auth/me")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_login_then_me() {
        let state = setup_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());

        // 1. Login.
        let req_body = serde_json::json!({
            "username": "admin",
            "password": "admin123",
        });
        let login_response = tower::ServiceExt::oneshot(
            app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(login_response.status(), StatusCode::OK);

        // Extract the session cookie.
        let set_cookie = login_response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        // Parse "chennix_session=<token>; ..."
        let token = set_cookie
            .split(';')
            .next()
            .unwrap()
            .trim();
        // token is "chennix_session=xxx"

        // 2. Call /me with the cookie.
        let me_response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("GET")
                .uri("/admin/api/auth/me")
                .header("cookie", token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(me_response.status(), StatusCode::OK);

        let body = axum::body::to_bytes(me_response.into_body(), usize::MAX).await.unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["user"]["username"], "admin");
    }

    #[tokio::test]
    async fn test_logout_clears_session() {
        let state = setup_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());

        // Login first.
        let req_body = serde_json::json!({
            "username": "admin",
            "password": "admin123",
        });
        let login_response = tower::ServiceExt::oneshot(
            app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

        let set_cookie = login_response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let token = set_cookie.split(';').next().unwrap().trim().to_string();

        // Verify session was created.
        assert_eq!(state.session_store.read().await.len(), 1);

        // Logout.
        let _ = tower::ServiceExt::oneshot(
            app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/auth/logout")
                .header("cookie", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

        // Session should be cleared.
        assert_eq!(state.session_store.read().await.len(), 0);

        // /me should now return 401.
        let me_response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("GET")
                .uri("/admin/api/auth/me")
                .header("cookie", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(me_response.status(), StatusCode::UNAUTHORIZED);
    }

    /// Regression test: admin-only routes (protected by both session_middleware
    /// and require_role) must work when accessed with a valid admin session.
    ///
    /// This catches a layer-ordering bug where `require_role` was applied as
    /// the outermost layer (running before `session_middleware`), causing all
    /// admin routes to return 401 because `AdminAuthContext` had not been
    /// injected yet.
    #[tokio::test]
    async fn test_admin_route_with_valid_session() {
        let state = setup_state().await;
        let app = admin_router(state.clone()).with_state(state.clone());

        // 1. Login as admin.
        let req_body = serde_json::json!({
            "username": "admin",
            "password": "admin123",
        });
        let login_response = tower::ServiceExt::oneshot(
            app.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/admin/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(login_response.status(), StatusCode::OK);

        // Extract the session cookie.
        let set_cookie = login_response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        let token = set_cookie.split(';').next().unwrap().trim().to_string();

        // 2. Access an admin-only route (e.g. GET /admin/api/users).
        //    This route requires both a valid session AND role >= 10.
        let users_response = tower::ServiceExt::oneshot(
            app,
            axum::http::Request::builder()
                .method("GET")
                .uri("/admin/api/users")
                .header("cookie", &token)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

        // Must NOT be 401 — admin role is 100 which is >= 10.
        assert_eq!(
            users_response.status(),
            StatusCode::OK,
            "admin route should succeed with valid admin session, \
             got {} — possible layer ordering bug (require_role before session_middleware)",
            users_response.status()
        );
    }
}
