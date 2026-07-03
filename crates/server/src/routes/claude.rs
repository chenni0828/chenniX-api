//! Task 26: `POST /v1/messages` — Claude-format entry point.
//!
//! The handler is a thin wrapper around `proxy_request` with
//! `EntryFormat::Claude`. Cross-format translation (Claude → OpenAI when
//! the upstream channel is OpenAI-compatible) is handled inside the
//! executor via `chennix-translator`.

use axum::{extract::State, Extension, Json};
use axum::response::Response;
use chennix_common::AuthContext;
use chennix_core::executor::EntryFormat;

use crate::middleware::auth::ApiError;
use crate::routes::proxy_request;
use crate::state::AppState;

pub async fn claude_messages(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    proxy_request(state, auth, EntryFormat::Claude, body)
        .await
        .map_err(ApiError::from)
}
