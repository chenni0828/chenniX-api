//! Admin API error type — converts to a JSON response with `{ error, code }`.
//!
//! All admin handlers return `Result<T, AdminError>`. Storage errors are
//! auto-converted via the `From<ProxyError>` impl.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

/// Error type for all admin API handlers.
///
/// Maps to the JSON format `{"error": "...", "code": <http_status>}`.
#[derive(Debug)]
pub enum AdminError {
    /// 404 — resource not found.
    NotFound(String),
    /// 400 — invalid request body or parameters.
    BadRequest(String),
    /// 401 — not authenticated (no valid session).
    Unauthorized(String),
    /// 403 — authenticated but insufficient permissions.
    Forbidden(String),
    /// 502 — upstream error (e.g. failed to reach provider).
    BadGateway(String),
    /// 500 — internal server / storage error.
    Internal(String),
}

/// Auto-convert `ProxyError` into `AdminError`.
///
/// `ModelNotFound` → `NotFound`, `InvalidRequest` → `BadRequest`,
/// everything else → `Internal`.
impl From<chennix_common::ProxyError> for AdminError {
    fn from(e: chennix_common::ProxyError) -> Self {
        match e {
            chennix_common::ProxyError::ModelNotFound(msg) => Self::NotFound(msg),
            chennix_common::ProxyError::InvalidRequest(msg) => Self::BadRequest(msg),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl IntoResponse for AdminError {
    fn into_response(self) -> Response {
        let (status, msg) = match self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            Self::BadRequest(msg) => (StatusCode::BAD_REQUEST, msg),
            Self::Unauthorized(msg) => (StatusCode::UNAUTHORIZED, msg),
            Self::Forbidden(msg) => (StatusCode::FORBIDDEN, msg),
            Self::BadGateway(msg) => (StatusCode::BAD_GATEWAY, msg),
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        let code = status.as_u16();
        (status, Json(json!({ "error": msg, "code": code }))).into_response()
    }
}

/// Convenience alias: all admin handlers return `Result<T, AdminError>`.
pub type AdminResult<T> = Result<T, AdminError>;
