//! Admin API route registration.
//!
//! Builds the admin router with three groups:
//! 1. **Public** — `/admin/api/auth/login` (no session required).
//! 2. **User** — routes available to all authenticated users (session
//!    middleware only): auth (logout/me), dashboard, self-service password
//!    change, own token CRUD, own usage/logs.
//! 3. **Admin** — routes restricted to admins (role >= 10): user/channel/key
//!    model management, reload, and connection tests. Protected by both
//!    `session_middleware` and `require_role(10)`.

use axum::routing::{delete, get, patch, post, put};
use axum::Router;

use crate::admin::auth::{login_handler, logout_handler, me_handler, session_middleware};
use crate::admin::handlers::*;
use crate::admin::setup;
use crate::middleware::auth::require_role;
use crate::state::AppState;

/// Build the complete admin API router (stateless — caller must apply state).
///
/// Call this from `main.rs`, merge into the main router, then call
/// `.with_state(state)`. For tests, call `.with_state(state)` on the
/// returned router before using `oneshot`.
pub fn admin_router(state: AppState) -> Router<AppState> {
    // Public routes — no session required.
    // Includes: login, setup wizard (status + initialize).
    let public_routes = Router::<AppState>::new()
        .route("/admin/api/auth/login", post(login_handler))
        .route("/admin/api/setup/status", get(setup::setup_status_handler))
        .route("/admin/api/setup/initialize", post(setup::setup_initialize_handler));

    // User routes — session middleware only (any logged-in user).
    // Includes: auth (logout/me), dashboard, self-service password change,
    // own token management, own usage/logs.
    let user_routes = Router::<AppState>::new()
        // Auth (session required)
        .route("/admin/api/auth/logout", post(logout_handler))
        .route("/admin/api/auth/me", get(me_handler))
        // Dashboard
        .route("/admin/api/dashboard", get(dashboard_handler))
        // Self-service password change
        .route("/admin/api/me/password", put(update_my_password_handler))
        // Tokens CRUD (ownership enforced in handlers)
        .route("/admin/api/tokens", get(list_tokens_handler).post(create_token_handler))
        .route(
            "/admin/api/tokens/:id",
            put(update_token_handler).delete(delete_token_handler),
        )
        .route("/admin/api/tokens/:id/usage", get(token_usage_handler))
        // Usage & Logs (filtered by user_id for non-admins in handlers)
        .route("/admin/api/usage", get(usage_handler))
        .route("/admin/api/logs", get(logs_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            session_middleware,
        ));

    // Admin routes — session middleware + require_role(10).
    // Includes: user/channel/key/model management, reload, connection tests.
    //
    // IMPORTANT: layer order matters.  In axum the LAST `.layer()` call
    // becomes the OUTERMOST layer (runs first on the request).  We need
    // `session_middleware` to run BEFORE `require_role` so that the
    // `AdminAuthContext` is injected before the role check reads it.
    // Therefore `require_role` is added first (inner) and
    // `session_middleware` second (outer).
    let admin_routes = Router::<AppState>::new()
        // Users CRUD
        .route("/admin/api/users", get(list_users_handler).post(create_user_handler))
        .route(
            "/admin/api/users/:id",
            put(update_user_handler).delete(delete_user_handler),
        )
        .route("/admin/api/users/:id/password", put(update_password_handler))
        // Channels CRUD
        .route(
            "/admin/api/channels",
            get(list_channels_handler).post(create_channel_handler),
        )
        .route(
            "/admin/api/channels/:id",
            put(update_channel_handler).delete(delete_channel_handler),
        )
        .route("/admin/api/channels/:id/test", post(test_channel_handler))
        .route("/admin/api/discover-models", post(discover_models_by_form_handler))
        .route("/admin/api/channels/:id/discover-models", post(discover_channel_models_handler))
        .route(
            "/admin/api/channels/:id/discovered-models",
            post(add_discovered_models_handler).delete(delete_discovered_model_handler),
        )
        .route(
            "/admin/api/channels/:id/models",
            get(list_channel_models_handler),
        )
        // Small-model quota management
        .route("/admin/api/small-models", get(list_small_models_handler))
        .route(
            "/admin/api/channels/:id/models/quota",
            patch(update_small_model_quota_handler),
        )
        .route(
            "/admin/api/channels/:id/models/quota/reset",
            post(reset_small_model_quota_handler),
        )
        // Keys CRUD (nested under channels)
        .route(
            "/admin/api/channels/:id/keys",
            get(list_keys_handler).post(create_key_handler),
        )
        .route(
            "/admin/api/channels/:id/keys/:kid",
            put(update_key_handler).delete(delete_key_handler),
        )
        .route(
            "/admin/api/channels/:id/keys/:kid/reset-quota",
            post(reset_key_quota_handler),
        )
        // Models CRUD
        .route("/admin/api/models", get(list_models_handler).post(create_model_handler))
        .route(
            "/admin/api/models/:id",
            put(update_model_handler).delete(delete_model_handler),
        )
        .route("/admin/api/models/:id/strategy", patch(update_routing_strategy_handler))
        .route("/admin/api/models/:id/pricing", put(update_binding_pricing_handler))
        .route("/admin/api/models/:id/test", post(test_model_handler))
        // Pricing — channel-model level
        .route("/admin/api/pricing", get(list_all_pricing_handler))
        // Model-channel bindings
        .route("/admin/api/models/:id/bindings", post(add_binding_handler))
        .route(
            "/admin/api/models/:id/bindings/weight",
            patch(update_binding_weight_handler),
        )
        .route(
            "/admin/api/models/:id/bindings/reorder",
            put(reorder_bindings_handler),
        )
        .route(
            "/admin/api/models/:id/bindings/remove",
            delete(remove_binding_handler),
        )
        .route(
            "/admin/api/models/:id/bindings/test",
            post(test_binding_handler),
        )
        // Reload
        .route("/admin/api/reload", post(reload_handler))
        .layer(require_role(10))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            session_middleware,
        ));

    Router::new()
        .merge(public_routes)
        .merge(user_routes)
        .merge(admin_routes)
}
