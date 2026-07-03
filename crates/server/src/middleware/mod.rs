//! Middleware implementations: token auth + request logging.
//!
//! The auth middleware extracts the Bearer token from the Authorization
//! header, validates it via `TokenRepo::validate_token`, and injects an
//! `AuthContext` into the request extensions. Route handlers retrieve it
//! via `Extension(auth): Extension<AuthContext>`.

pub mod auth;
pub mod logging;

// re-export the auth middleware for convenience
pub use auth::token_auth_middleware;
pub use logging::request_log_layer;
