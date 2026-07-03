//! Task 24: Token authentication middleware.
//!
//! Flow:
//! 1. Extract `Authorization: Bearer sk-xxx` from the request header.
//! 2. Determine the client IP (x-forwarded-for / x-real-ip / connection).
//! 3. Call `TokenRepo::validate_token(key, client_ip)`.
//! 4. On success: insert `AuthContext` into `req.extensions_mut()`.
//! 5. On failure: return 401 with a JSON error body.
//! 6. Call `next.run(req)`.

use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;
use tracing::debug;
use tower::{Layer, Service};

use chennix_common::{AdminAuthContext, AuthContext, ProxyError};
use chennix_storage::tokens::TokenRepo;

use crate::state::AppState;

/// The middleware function. Applied to every request via
/// `Router::layer(middleware::from_fn_with_state(state, token_auth_middleware))`.
pub async fn token_auth_middleware(
    State(state): State<AppState>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    // 1. Extract Bearer token
    let token_key = match extract_bearer(&req) {
        Some(t) => t,
        None => return auth_error("missing or invalid Authorization header"),
    };

    // 2. Determine client IP
    let client_ip = extract_client_ip(&req);

    // 3. Validate
    let auth_ctx = {
        let db = state.db.lock().await;
        let repo = TokenRepo::new(&db);
        match repo.validate_token(&token_key, client_ip.as_deref()) {
            Ok(Some(ctx)) => ctx,
            Ok(None) => {
                debug!("token validation failed for key prefix={}", &token_key[..token_key.len().min(8)]);
                return auth_error("invalid or expired token");
            }
            Err(e) => {
                tracing::error!("token validation storage error: {}", e);
                return internal_error("auth storage error");
            }
        }
    };

    // 4. Inject AuthContext + call next
    req.extensions_mut().insert(auth_ctx);
    next.run(req).await
}

/// Extract `Bearer <token>` from the Authorization header.
fn extract_bearer(req: &Request<Body>) -> Option<String> {
    let header = req.headers().get(axum::http::header::AUTHORIZATION)?;
    let value = header.to_str().ok()?;
    let token = value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))?;
    let trimmed = token.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Determine the client IP. Checks `x-forwarded-for` (first entry) and
/// `x-real-ip`, falling back to `None` (the connection-level remote addr
/// is not directly available in axum 0.7 without a custom ConnectInfo
/// layer; this is sufficient for IP whitelist validation which treats
/// `None` as "no IP available").
fn extract_client_ip(req: &Request<Body>) -> Option<String> {
    if let Some(xff) = req.headers().get("x-forwarded-for") {
        if let Ok(s) = xff.to_str() {
            if let Some(first) = s.split(',').next() {
                let ip = first.trim();
                if !ip.is_empty() {
                    return Some(ip.to_string());
                }
            }
        }
    }
    if let Some(xri) = req.headers().get("x-real-ip") {
        if let Ok(s) = xri.to_str() {
            let ip = s.trim();
            if !ip.is_empty() {
                return Some(ip.to_string());
            }
        }
    }
    None
}

fn auth_error(msg: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({
            "error": {
                "type": "authentication_error",
                "message": msg,
            }
        })),
    )
        .into_response()
}

fn internal_error(msg: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "error": {
                "type": "internal_error",
                "message": msg,
            }
        })),
    )
        .into_response()
}

/// Role-based access control middleware factory.
///
/// Returns a middleware layer that checks whether the authenticated user's
/// `role` is >= `min_role`. The role is read from either `AdminAuthContext`
/// (admin panel session auth) or `AuthContext` (proxy Bearer-token auth),
/// whichever is present in the request extensions.
///
/// If the role is insufficient, responds with **403 Forbidden**.
/// If no auth context is present (the auth middleware was not applied),
/// responds with **401 Unauthorized**.
///
/// This layer must be applied **after** `session_middleware` (admin) or
/// `token_auth_middleware` (proxy) so that the auth context is already
/// injected into the request extensions.
///
/// # Role levels
/// - `1`   — common user
/// - `10`  — admin
/// - `100` — root
pub fn require_role(min_role: i32) -> RequireRoleLayer {
    RequireRoleLayer { min_role }
}

/// Layer produced by [`require_role`].
#[derive(Clone)]
pub struct RequireRoleLayer {
    min_role: i32,
}

impl<S> Layer<S> for RequireRoleLayer {
    type Service = RequireRoleService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        RequireRoleService {
            inner,
            min_role: self.min_role,
        }
    }
}

/// Service produced by [`RequireRoleLayer`].
#[derive(Clone)]
pub struct RequireRoleService<S> {
    inner: S,
    min_role: i32,
}

impl<S> Service<Request<Body>> for RequireRoleService<S>
where
    S: Service<Request<Body>, Response = Response> + Clone + Send + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        let min_role = self.min_role;
        let mut inner = self.inner.clone();
        Box::pin(async move {
            // Check AdminAuthContext first (admin panel routes with session auth)
            if let Some(ctx) = req.extensions().get::<AdminAuthContext>() {
                if ctx.user.role >= min_role {
                    return inner.call(req).await;
                } else {
                    return Ok(forbidden_error("insufficient role for this resource"));
                }
            }
            // Fall back to AuthContext (proxy routes with Bearer token auth)
            match req.extensions().get::<AuthContext>() {
                Some(ctx) if ctx.user.role >= min_role => inner.call(req).await,
                Some(_) => Ok(forbidden_error("insufficient role for this resource")),
                None => Ok(auth_error("not authenticated")),
            }
        })
    }
}

fn forbidden_error(msg: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": {
                "type": "authorization_error",
                "message": msg,
            }
        })),
    )
        .into_response()
}

/// Convert a `ProxyError` into an axum `Response` with a JSON body.
/// Implemented here (not in chennix-common) because `IntoResponse` is an
/// axum trait and chennix-common does not depend on axum.
pub fn proxy_error_response(e: &ProxyError) -> Response {
    let status = StatusCode::from_u16(e.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        Json(json!({
            "error": {
                "type": error_type_name(e),
                "message": e.to_string(),
            }
        })),
    )
        .into_response()
}

fn error_type_name(e: &ProxyError) -> &'static str {
    match e {
        ProxyError::ClientAuthFailed => "authentication_error",
        ProxyError::ModelNotFound(_) => "not_found_error",
        ProxyError::AllKeysDisabled { .. } => "service_unavailable",
        ProxyError::AllKeysCooldown { .. } => "service_unavailable",
        ProxyError::AllKeysQuotaExhausted { .. } => "service_unavailable",
        ProxyError::AllKeysExhausted { .. } => "service_unavailable",
        ProxyError::Upstream { .. } => "upstream_error",
        ProxyError::InvalidRequest(_) => "invalid_request_error",
        ProxyError::Translator(_) => "translator_error",
        ProxyError::Storage(_) => "internal_error",
        ProxyError::Config(_) => "config_error",
        ProxyError::Io(_) => "internal_error",
        ProxyError::Json(_) => "internal_error",
        ProxyError::Http(_) => "internal_error",
        ProxyError::UpstreamTimeout(_) => "timeout_error",
    }
}

/// Newtype wrapper around `ProxyError` so we can implement `IntoResponse`
/// (the orphan rule forbids implementing an external trait for an
/// external type). Route handlers return `Result<T, ApiError>`; `?`
/// works automatically via the `From<ProxyError>` impl.
#[derive(Debug)]
pub struct ApiError(pub ProxyError);

impl From<ProxyError> for ApiError {
    fn from(e: ProxyError) -> Self {
        Self(e)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        proxy_error_response(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn make_req(auth: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().uri("/v1/models").body(Body::empty()).unwrap();
        if let Some(a) = auth {
            builder
                .headers_mut()
                .insert(axum::http::header::AUTHORIZATION, HeaderValue::from_str(a).unwrap());
        }
        builder
    }

    #[test]
    fn test_extract_bearer_valid() {
        let req = make_req(Some("Bearer sk-test123"));
        assert_eq!(extract_bearer(&req).as_deref(), Some("sk-test123"));
    }

    #[test]
    fn test_extract_bearer_case_insensitive_prefix() {
        let req = make_req(Some("bearer sk-lower"));
        assert_eq!(extract_bearer(&req).as_deref(), Some("sk-lower"));
    }

    #[test]
    fn test_extract_bearer_missing_header() {
        let req = make_req(None);
        assert!(extract_bearer(&req).is_none());
    }

    #[test]
    fn test_extract_bearer_wrong_scheme() {
        let req = make_req(Some("Basic dXNlcjpwYXNz"));
        assert!(extract_bearer(&req).is_none());
    }

    #[test]
    fn test_extract_bearer_empty_token() {
        let req = make_req(Some("Bearer "));
        assert!(extract_bearer(&req).is_none());
    }

    #[test]
    fn test_extract_bearer_trimmed() {
        let req = make_req(Some("Bearer   sk-with-spaces  "));
        assert_eq!(extract_bearer(&req).as_deref(), Some("sk-with-spaces"));
    }

    #[test]
    fn test_extract_client_ip_xff_first() {
        let mut req = Request::builder().body(Body::empty()).unwrap();
        req.headers_mut().insert(
            "x-forwarded-for",
            HeaderValue::from_static("1.2.3.4, 5.6.7.8"),
        );
        assert_eq!(extract_client_ip(&req).as_deref(), Some("1.2.3.4"));
    }

    #[test]
    fn test_extract_client_ip_xri() {
        let mut req = Request::builder().body(Body::empty()).unwrap();
        req.headers_mut()
            .insert("x-real-ip", HeaderValue::from_static("9.9.9.9"));
        assert_eq!(extract_client_ip(&req).as_deref(), Some("9.9.9.9"));
    }

    #[test]
    fn test_extract_client_ip_none() {
        let req = Request::builder().body(Body::empty()).unwrap();
        assert!(extract_client_ip(&req).is_none());
    }

    #[test]
    fn test_extract_client_ip_xff_takes_priority_over_xri() {
        let mut req = Request::builder().body(Body::empty()).unwrap();
        req.headers_mut()
            .insert("x-forwarded-for", HeaderValue::from_static("1.1.1.1"));
        req.headers_mut()
            .insert("x-real-ip", HeaderValue::from_static("2.2.2.2"));
        assert_eq!(extract_client_ip(&req).as_deref(), Some("1.1.1.1"));
    }

    #[test]
    fn test_proxy_error_response_status() {
        let resp = proxy_error_response(&ProxyError::ClientAuthFailed);
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = proxy_error_response(&ProxyError::ModelNotFound("gpt-4".into()));
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let resp = proxy_error_response(&ProxyError::AllKeysExhausted {
            model: "x".into(),
            attempted_keys: vec![],
            last_error: None,
        });
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);

        let resp = proxy_error_response(&ProxyError::InvalidRequest("bad".into()));
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}
