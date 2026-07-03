//! Task 25: `POST /v1/chat/completions` — OpenAI-format entry point.
//!
//! The handler is a thin wrapper around `proxy_request` with
//! `EntryFormat::OpenAI`. All the heavy lifting (model resolution,
//! routing, translation, billing, streaming) happens in the shared
//! pipeline.

use axum::{extract::State, Extension, Json};
use axum::response::Response;
use chennix_common::AuthContext;
use chennix_core::executor::EntryFormat;

use crate::middleware::auth::ApiError;
use crate::routes::proxy_request;
use crate::state::AppState;

pub async fn chat_completions(
    State(state): State<AppState>,
    Extension(auth): Extension<AuthContext>,
    Json(body): Json<serde_json::Value>,
) -> Result<Response, ApiError> {
    proxy_request(state, auth, EntryFormat::OpenAI, body)
        .await
        .map_err(ApiError::from)
}
