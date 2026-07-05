//! Task 27: `GET /v1/models` — list available models.
//!
//! Returns an OpenAI-format model list:
//! ```json
//! {
//!   "object": "list",
//!   "data": [
//!     { "id": "gpt-4", "object": "model", "created": 0, "owned_by": "chennix" },
//!     ...
//!   ]
//! }
//! ```
//!
//! If the token has `model_limits_enabled`, the list is filtered to only
//! the allowed models (matched by canonical name).

use axum::{extract::State, Extension, Json};
use chennix_common::AuthContext;
use chennix_storage::models::ModelRepo;
use serde_json::{json, Value};

use crate::middleware::auth::ApiError;
use crate::state::AppState;

pub async fn list_models(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
) -> Result<Json<Value>, ApiError> {
    // 1. Get all models from storage
    let models = {
        let db = state.db.lock().await;
        let repo = ModelRepo::new(&db);
        repo.list_all_models()?
    };

    // 2. Filter by token model_limits if enabled
    let filtered: Vec<(i64, String, String)> = if auth.token.model_limits_enabled {
        match &auth.token.model_limits {
            Some(limits) if !limits.is_empty() => models
                .into_iter()
                .filter(|(_, name, _)| limits.iter().any(|l| l.eq_ignore_ascii_case(name)))
                .collect(),
            // empty limits list when enabled → nothing allowed
            Some(_) => Vec::new(),
            // model_limits is None but enabled → treat as "all allowed"
            None => models,
        }
    } else {
        models
    };

    // 3. Build OpenAI-format response
    let data: Vec<Value> = filtered
        .into_iter()
        .map(|(_id, name, _)| {
            json!({
                "id": name,
                "object": "model",
                "created": 0,
                "owned_by": "chennix",
            })
        })
        .collect();

    Ok(Json(json!({
        "object": "list",
        "data": data,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use chennix_common::{TokenConfig, UserConfig};
    use chennix_storage::schema::init_db;
    use rusqlite::Connection;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    async fn setup_state_with_models(models: &[&str]) -> AppState {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let model_repo = ModelRepo::new(&conn);
        for m in models {
            model_repo.create_model(m).unwrap();
        }
        // create a user + token
        conn.execute(
            "INSERT INTO users (id, username, password_hash, role, status, quota, used_quota, \"group\")
             VALUES (1, 'alice', 'h', 1, 1, 1000, 0, 'default')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tokens (id, user_id, key, remain_quota, used_quota, unlimited_quota,
                                 expired_time, model_limits_enabled, status)
             VALUES (1, 1, 'sk-alice', 500, 0, 0, -1, 0, 1)",
            [],
        )
        .unwrap();

        let db = Arc::new(Mutex::new(conn));
        let storage = Arc::new(crate::pipeline::StorageAdapter::new(db.clone()));
        let normalizer = Arc::new(chennix_core::normalizer::Normalizer::new());
        let cache = Arc::new(chennix_core::cache::ConfigCache::new(normalizer.clone()));
        let health = Arc::new(chennix_core::health::HealthManager::with_db(db.clone()));
        let http_client = reqwest::Client::new();
        let executor = Arc::new(chennix_core::executor::Executor::new(
            health.clone(),
            cache.clone(),
            http_client.clone(),
            std::time::Duration::from_secs(60),
            std::time::Duration::from_secs(300),
        ));

        let config = Arc::new(crate::config::AppConfig {
            server: crate::config::ServerConfig {
                host: "0.0.0.0".into(),
                port: 8080,
                tls: crate::config::TlsConfig::default(),
                upstream_timeout_secs: 60,
                streaming_timeout_secs: 300,
            },
            log: crate::config::LogConfig::default(),
            bootstrap: crate::config::BootstrapConfig {
                config_file: "".into(),
            },
            database: crate::config::DatabaseConfig::default(),
        });

        AppState {
            executor,
            cache,
            health,
            normalizer,
            storage,
            db,
            active_streams: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            config,
            session_store: crate::admin::new_session_store(),
        }
    }

    fn auth_ctx(model_limits_enabled: bool, limits: Option<Vec<String>>) -> AuthContext {
        AuthContext {
            user: UserConfig {
                id: 1,
                username: "alice".into(),
                role: 1,
                status: 1,
                quota: 1000,
                used_quota: 0,
                group: "default".into(),
            },
            token: TokenConfig {
                id: 1,
                user_id: 1,
                key: "sk-alice".into(),
                name: None,
                remain_quota: 500,
                used_quota: 0,
                unlimited_quota: false,
                expired_time: -1,
                model_limits_enabled,
                model_limits: limits,
                status: 1,
                allow_ips: None,
            },
            client_ip: None,
        }
    }

    #[tokio::test]
    async fn test_list_models_no_limits() {
        let state = setup_state_with_models(&["gpt-4", "claude-3", "deepseek-v3"]).await;
        let auth = auth_ctx(false, None);

        let resp = list_models(State(state), Extension(auth)).await.unwrap();
        let json = resp.0;
        assert_eq!(json["object"], "list");
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 3);
        let names: Vec<&str> = data.iter().map(|d| d["id"].as_str().unwrap()).collect();
        assert!(names.contains(&"gpt-4"));
        assert!(names.contains(&"claude-3"));
        assert!(names.contains(&"deepseek-v3"));
    }

    #[tokio::test]
    async fn test_list_models_with_limits() {
        let state = setup_state_with_models(&["gpt-4", "claude-3", "deepseek-v3"]).await;
        let auth = auth_ctx(true, Some(vec!["gpt-4".into()]));

        let resp = list_models(State(state), Extension(auth)).await.unwrap();
        let json = resp.0;
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1, "only gpt-4 should be listed");
        assert_eq!(data[0]["id"], "gpt-4");
    }

    #[tokio::test]
    async fn test_list_models_empty_limits_when_enabled() {
        let state = setup_state_with_models(&["gpt-4", "claude-3"]).await;
        let auth = auth_ctx(true, Some(vec![]));

        let resp = list_models(State(state), Extension(auth)).await.unwrap();
        let json = resp.0;
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 0, "empty limits list when enabled → nothing allowed");
    }

    #[tokio::test]
    async fn test_list_models_limits_case_insensitive() {
        let state = setup_state_with_models(&["GPT-4", "Claude-3"]).await;
        let auth = auth_ctx(true, Some(vec!["gpt-4".into()]));

        let resp = list_models(State(state), Extension(auth)).await.unwrap();
        let json = resp.0;
        let data = json["data"].as_array().unwrap();
        assert_eq!(data.len(), 1, "case-insensitive match should find GPT-4");
        assert_eq!(data[0]["id"], "GPT-4");
    }

    #[tokio::test]
    async fn test_list_models_empty_db() {
        let state = setup_state_with_models(&[]).await;
        let auth = auth_ctx(false, None);

        let resp = list_models(State(state), Extension(auth)).await.unwrap();
        let json = resp.0;
        assert_eq!(json["object"], "list");
        assert_eq!(json["data"].as_array().unwrap().len(), 0);
    }

    // Suppress unused import warning for Request/Body (kept for future
    // integration tests that may construct full HTTP requests).
    #[allow(dead_code)]
    fn _ensure_imports_used() {
        let _ = Request::builder().body(Body::empty()).unwrap();
    }
}
