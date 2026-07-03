//! Admin API module — session authentication + management CRUD + data queries.
//!
//! ## Structure
//! - [`auth`] — `SessionStore`, login/logout/me handlers, session middleware.
//! - [`error`] — `AdminError` type (converts to `{ error, code }` JSON).
//! - [`handlers`] — all CRUD + dashboard + usage + logs + reload handlers.
//! - [`routes`] — `admin_router()` builder that wires everything together.
//!
//! ## Usage
//! In `main.rs`:
//! ```ignore
//! let admin = chennix_server::admin::admin_router(state.clone());
//! let app = Router::new()
//!     .merge(proxy_routes)
//!     .merge(admin)
//!     .merge(public_routes);
//! ```

pub mod auth;
pub mod error;
pub mod handlers;
pub mod routes;
pub mod setup;

pub use auth::new_session_store;
pub use routes::admin_router;
