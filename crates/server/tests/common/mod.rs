#![allow(dead_code)]
//! Shared test helpers for integration tests.
//!
//! Provides:
//! - `TestEnv` — encapsulates the axum Router, AppState, DB, and mock servers
//! - `setup()` — creates a standard test environment with seeded data
//! - Request helpers (`send_chat_request`, `send_claude_request`)
//! - Mock helpers (`mock_openai_ok`, `mock_claude_ok`, etc.)
//! - DB query helpers for verifying billing/usage state

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
    Router,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower::ServiceExt;
use tower_http::cors::CorsLayer;
use wiremock::{Mock, MockServer, ResponseTemplate};
use wiremock::matchers::{header, method, path};

use chennix_core::cache::ConfigCache;
use chennix_core::executor::Executor;
use chennix_core::health::HealthManager;
use chennix_core::normalizer::Normalizer;
use chennix_storage::schema::init_db;

use chennix_server::config::AppConfig;
use chennix_server::middleware::token_auth_middleware;
use chennix_server::pipeline::StorageAdapter;
use chennix_server::routes::{claude::claude_messages, models::list_models, openai::chat_completions};
use chennix_server::state::{AppState, SharedDb};

// ---- Test data constants ----

pub const TOKEN1: &str = "sk-token1"; // user1, group=default
pub const TOKEN2: &str = "sk-token2"; // user2, group=premium

pub const UPSTREAM_KEY1: &str = "sk-up-key1"; // channel1, free
pub const UPSTREAM_KEY2: &str = "sk-up-key2"; // channel1, paid
pub const UPSTREAM_KEY3: &str = "sk-up-key3"; // channel2, free
pub const UPSTREAM_KEY4: &str = "sk-up-key4"; // channel2, paid

pub const USER1_ID: i64 = 1;
pub const USER2_ID: i64 = 2;
pub const TOKEN1_ID: i64 = 1;
pub const TOKEN2_ID: i64 = 2;
pub const CHANNEL1_ID: i64 = 1; // openai-compatible, group=default
pub const CHANNEL2_ID: i64 = 2; // anthropic, group=premium
pub const KEY1_ID: i64 = 1;
pub const KEY2_ID: i64 = 2;
pub const MODEL_GPT4O_ID: i64 = 1;
pub const MODEL_CLAUDE_ID: i64 = 2;

// ---- TestEnv ----

pub struct TestEnv {
    pub app: Router,
    pub state: AppState,
    pub db: SharedDb,
    pub mock_openai: MockServer,
    pub mock_claude: MockServer,
}

impl TestEnv {
    pub async fn db(&self) -> tokio::sync::MutexGuard<'_, Connection> {
        self.db.lock().await
    }
}

/// Create a standard test environment with two mock upstream servers
/// and a seeded in-memory SQLite database.
pub async fn setup() -> TestEnv {
    let mock_openai = MockServer::start().await;
    let mock_claude = MockServer::start().await;

    let db = setup_test_db(&mock_openai.uri(), &mock_claude.uri());
    let (app, state) = create_test_app(db.clone());

    TestEnv {
        app,
        state,
        db,
        mock_openai,
        mock_claude,
    }
}

/// Seed an in-memory SQLite database with standard test data.
fn setup_test_db(openai_url: &str, claude_url: &str) -> SharedDb {
    let conn = Connection::open_in_memory().unwrap();
    init_db(&conn).unwrap();

    // --- Users ---
    // quota 单位为微元（1 元 = 1_000_000 微元）。10000 元 = 10_000_000_000 微元。
    conn.execute(
        "INSERT INTO users (id, username, password_hash, role, status, quota, used_quota, \"group\")
         VALUES (1, 'user1', 'h', 1, 1, 10000000000, 0, 'default')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO users (id, username, password_hash, role, status, quota, used_quota, \"group\")
         VALUES (2, 'user2', 'h', 1, 1, 10000000000, 0, 'premium')",
        [],
    )
    .unwrap();

    // --- Tokens ---
    // remain_quota 单位为微元。5000 元 = 5_000_000_000 微元。
    conn.execute(
        "INSERT INTO tokens (id, user_id, key, remain_quota, used_quota, unlimited_quota,
                             expired_time, model_limits_enabled, status)
         VALUES (1, 1, 'sk-token1', 5000000000, 0, 0, -1, 0, 1)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tokens (id, user_id, key, remain_quota, used_quota, unlimited_quota,
                             expired_time, model_limits_enabled, status)
         VALUES (2, 2, 'sk-token2', 5000000000, 0, 0, -1, 0, 1)",
        [],
    )
    .unwrap();

    // --- Channels ---
    conn.execute(
        "INSERT INTO channels (id, name, provider, base_url, priority, \"group\")
         VALUES (1, 'ch-openai', 'openai-compatible', ?1, 100, 'default')",
        params![openai_url],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO channels (id, name, provider, base_url, priority, \"group\")
         VALUES (2, 'ch-claude', 'anthropic', ?1, 100, 'premium')",
        params![claude_url],
    )
    .unwrap();

    // --- Keys (2 per channel) ---
    conn.execute(
        "INSERT INTO channel_keys (id, channel_id, api_key, label, cost_tier, key_priority,
                                   price_per_1k_tokens, free_quota, status)
         VALUES (1, 1, 'sk-up-key1', 'key1', 'free', 100, 0.01, 1000, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO channel_keys (id, channel_id, api_key, label, cost_tier, key_priority,
                                   price_per_1k_tokens, free_quota, status)
         VALUES (2, 1, 'sk-up-key2', 'key2', 'paid', 100, 0.01, 1000, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO channel_keys (id, channel_id, api_key, label, cost_tier, key_priority,
                                   price_per_1k_tokens, free_quota, status)
         VALUES (3, 2, 'sk-up-key3', 'key3', 'free', 100, 0.01, 1000, 'active')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO channel_keys (id, channel_id, api_key, label, cost_tier, key_priority,
                                   price_per_1k_tokens, free_quota, status)
         VALUES (4, 2, 'sk-up-key4', 'key4', 'paid', 100, 0.01, 1000, 'active')",
        [],
    )
    .unwrap();

    // --- Models ---
    conn.execute(
        "INSERT INTO models (id, canonical_name) VALUES (1, 'gpt-4o')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO models (id, canonical_name) VALUES (2, 'claude-3-5-sonnet')",
        [],
    )
    .unwrap();

    // --- Bindings ---
    // gpt-4o → channel1 (default) + channel2 (premium) for group routing.
    // Pricing is set on each binding: input=0.01 元/1K, output=0.01 元/1K
    // (matches the historical key-level price_per_1k_tokens=0.01).
    conn.execute(
        "INSERT INTO model_channels (model_id, channel_id, upstream_model_name,
                                     billing_type, input_price, output_price)
         VALUES (1, 1, 'gpt-4o', 0, 0.01, 0.01)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO model_channels (model_id, channel_id, upstream_model_name,
                                     billing_type, input_price, output_price)
         VALUES (1, 2, 'gpt-4o', 0, 0.01, 0.01)",
        [],
    )
    .unwrap();
    // claude-3-5-sonnet → channel2
    conn.execute(
        "INSERT INTO model_channels (model_id, channel_id, upstream_model_name,
                                     billing_type, input_price, output_price)
         VALUES (2, 2, 'claude-3-5-sonnet', 0, 0.01, 0.01)",
        [],
    )
    .unwrap();

    Arc::new(Mutex::new(conn))
}

/// Build the axum Router + AppState from a shared DB connection.
fn create_test_app(db: SharedDb) -> (Router, AppState) {
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

    let config = Arc::new(AppConfig {
        server: chennix_server::config::ServerConfig {
            host: "0.0.0.0".into(),
            port: 8080,
            tls: chennix_server::config::TlsConfig::default(),
            upstream_timeout_secs: 60,
            streaming_timeout_secs: 300,
        },
        log: chennix_server::config::LogConfig::default(),
        bootstrap: chennix_server::config::BootstrapConfig {
            config_file: "".into(),
        },
        database: chennix_server::config::DatabaseConfig::default(),
    });

    let state = AppState {
        executor,
        cache,
        health,
        normalizer,
        storage,
        db,
        config,
        session_store: chennix_server::admin::new_session_store(),
        active_streams: Arc::new(AtomicUsize::new(0)),
        http_client,
    };

    let app = build_test_router(state.clone());
    (app, state)
}

/// Replicates `main.rs::build_router` but without the public `/health` route
/// (not needed for integration tests).
fn build_test_router(state: AppState) -> Router {
    let authed_routes = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(claude_messages))
        .route("/v1/models", get(list_models))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            token_auth_middleware,
        ));

    Router::new()
        .merge(authed_routes)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// ---- Request helpers ----

/// Send an OpenAI-format chat completion request.
pub async fn send_chat_request(
    app: &Router,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Send a Claude-format messages request.
pub async fn send_claude_request(
    app: &Router,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/messages")
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Send a streaming chat completion request, returning the raw response.
pub async fn send_stream_request(
    app: &Router,
    token: &str,
    body: Value,
) -> (StatusCode, Vec<u8>) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();

    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, bytes.to_vec())
}

// ---- Mock helpers ----

/// Mock a successful OpenAI upstream response.
pub async fn mock_openai_ok(server: &MockServer, prompt: u64, completion: u64) {
    let total = prompt + completion;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "model": "gpt-4o",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Mock response from OpenAI upstream"},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": prompt,
                        "completion_tokens": completion,
                        "total_tokens": total
                    }
                })),
        )
        .mount(server)
        .await;
}

/// Mock a successful OpenAI upstream response with custom content.
pub async fn mock_openai_ok_with_content(server: &MockServer, content: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "id": "chatcmpl-test",
                    "object": "chat.completion",
                    "model": "gpt-4o",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": content},
                        "finish_reason": "stop"
                    }],
                    "usage": {
                        "prompt_tokens": 100,
                        "completion_tokens": 50,
                        "total_tokens": 150
                    }
                })),
        )
        .mount(server)
        .await;
}

/// Mock a successful Claude upstream response.
pub async fn mock_claude_ok(server: &MockServer, input: u64, output: u64) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "id": "msg_test",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-3-5-sonnet",
                    "content": [{"type": "text", "text": "Mock response from Claude upstream"}],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": input,
                        "output_tokens": output
                    }
                })),
        )
        .mount(server)
        .await;
}

/// Mock a successful Claude upstream response with custom content.
pub async fn mock_claude_ok_with_content(server: &MockServer, content: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "id": "msg_test",
                    "type": "message",
                    "role": "assistant",
                    "model": "claude-3-5-sonnet",
                    "content": [{"type": "text", "text": content}],
                    "stop_reason": "end_turn",
                    "usage": {
                        "input_tokens": 100,
                        "output_tokens": 50
                    }
                })),
        )
        .mount(server)
        .await;
}

/// Mock an OpenAI 429 rate limit for a specific API key.
pub async fn mock_openai_429_for_key(server: &MockServer, api_key: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("Authorization", format!("Bearer {}", api_key)))
        .respond_with(
            ResponseTemplate::new(429)
                .set_body_json(json!({
                    "error": {"type": "rate_limit_error", "message": "Rate limit exceeded"}
                })),
        )
        .mount(server)
        .await;
}

/// Mock an OpenAI 200 success for a specific API key.
pub async fn mock_openai_ok_for_key(server: &MockServer, api_key: &str) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("Authorization", format!("Bearer {}", api_key)))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({
                    "id": "chatcmpl-retry",
                    "object": "chat.completion",
                    "model": "gpt-4o",
                    "choices": [{
                        "index": 0,
                        "message": {"role": "assistant", "content": "Success after retry"},
                        "finish_reason": "stop"
                    }],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}
                })),
        )
        .mount(server)
        .await;
}

/// Mock an OpenAI SSE streaming response.
pub async fn mock_openai_stream(server: &MockServer) {
    let sse_body = concat!(
        "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"Hello\"}}]}\n\n",
        "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" world\"}}]}\n\n",
        "data: {\"id\":\"chat-1\",\"object\":\"chat.completion.chunk\",\"choices\":[],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5,\"total_tokens\":15}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_raw(sse_body.as_bytes(), "text/event-stream"),
        )
        .mount(server)
        .await;
}

// ---- DB query helpers ----

/// Get a user's used_quota from the DB.
pub fn get_user_used_quota(conn: &Connection, user_id: i64) -> i64 {
    conn.query_row(
        "SELECT used_quota FROM users WHERE id = ?1",
        params![user_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// Get a user's remaining quota (quota - used_quota).
pub fn get_user_remaining_quota(conn: &Connection, user_id: i64) -> i64 {
    conn.query_row(
        "SELECT quota - used_quota FROM users WHERE id = ?1",
        params![user_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// Get a token's remain_quota.
pub fn get_token_remain(conn: &Connection, token_id: i64) -> i64 {
    conn.query_row(
        "SELECT remain_quota FROM tokens WHERE id = ?1",
        params![token_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// Get a token's used_quota.
pub fn get_token_used(conn: &Connection, token_id: i64) -> i64 {
    conn.query_row(
        "SELECT used_quota FROM tokens WHERE id = ?1",
        params![token_id],
        |r| r.get(0),
    )
    .unwrap()
}

/// Count rows in usage_logs.
pub fn count_usage_logs(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM usage_logs", [], |r| r.get(0))
        .unwrap()
}

/// Get (user_id, token_id, key_id, quota_cost) from the first usage_log row.
pub fn get_first_usage_log(conn: &Connection) -> (i64, i64, i64, i64) {
    conn.query_row(
        "SELECT user_id, token_id, key_id, quota_cost FROM usage_logs ORDER BY id LIMIT 1",
        [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
    .unwrap()
}

/// Set a user's quota to a specific value.
pub fn set_user_quota(conn: &Connection, user_id: i64, quota: i64) {
    conn.execute(
        "UPDATE users SET quota = ?1 WHERE id = ?2",
        params![quota, user_id],
    )
    .unwrap();
}

/// Set a token's remain_quota to a specific value.
pub fn set_token_remain(conn: &Connection, token_id: i64, remain: i64) {
    conn.execute(
        "UPDATE tokens SET remain_quota = ?1 WHERE id = ?2",
        params![remain, token_id],
    )
    .unwrap();
}

/// Enable model limits on a token (only allow the specified models).
pub fn set_token_model_limits(conn: &Connection, token_id: i64, models: &[&str]) {
    let json = serde_json::to_string(models).unwrap();
    conn.execute(
        "UPDATE tokens SET model_limits_enabled = 1, model_limits = ?1 WHERE id = ?2",
        params![json, token_id],
    )
    .unwrap();
}

/// A basic OpenAI chat request body.
pub fn chat_body(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "Hello"}],
        "max_tokens": 10
    })
}

/// A basic Claude messages request body.
pub fn claude_body(model: &str) -> Value {
    json!({
        "model": model,
        "messages": [{"role": "user", "content": "Hello"}],
        "max_tokens": 10
    })
}
