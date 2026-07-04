//! chennix-server binary — the HTTP layer that ties all chennix-* crates together.
//!
//! Startup flow:
//! 1. Load YAML config.
//! 2. Open SQLite DB + init schema.
//! 3. Ensure default admin exists.
//! 4. Bootstrap import (if DB is empty and a bootstrap file is configured).
//! 5. Build AppState (normalizer, cache, health, executor, storage adapter).
//! 6. Build axum router with routes + auth middleware.
//! 7. Start the HTTP server (plain or TLS).
//! 8. Start a background health-recovery loop.

mod admin;
mod config;
mod middleware;
mod pipeline;
mod routes;
mod state;
mod static_files;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use tokio::signal;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

use chennix_core::cache::ConfigCache;
use chennix_core::executor::Executor;
use chennix_core::health::HealthManager;
use chennix_core::normalizer::Normalizer;
use chennix_storage::{bootstrap, open_db};

use crate::admin::{admin_router, new_session_store};
use crate::middleware::{request_log_layer, token_auth_middleware};
use crate::pipeline::StorageAdapter;
use crate::routes::{claude::claude_messages, models::list_models, openai::chat_completions};
use crate::state::AppState;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 0. Parse CLI args — expect a config path as the first argument.
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "config.yaml".to_string());

    // 1. Load config
    let config = config::load_config(&config_path)?;

    // 2. Init tracing
    init_tracing(&config.log.level);

    tracing::info!("chenniX-api starting (config={})", config_path);
    tracing::info!(
        "binding to {}:{} (tls={})",
        config.server.host,
        config.server.port,
        config.server.tls.enabled
    );

    // 3. Open DB + init schema
    let conn = open_db(&config.database.path)?;
    // Apply v1→v2 migration (no-op on fresh v2 databases).
    chennix_storage::schema::migrate_v1_to_v2(&conn)?;
    // Apply v2→v3 migration: model_channels 3-tuple PK + weight,
    // models.routing_strategy, discovered_models quota columns
    // (no-op on fresh v3 databases).
    chennix_storage::schema::migrate_v2_to_v3(&conn)?;
    // Apply v3→v4 migration: convert money-quota fields to micro-yuan
    // (×1,000,000) for integer-precision billing. Idempotent via
    // schema_meta marker table.
    chennix_storage::schema::migrate_v3_to_v4(&conn)?;

    // 4. Ensure default admin
    config::ensure_default_admin(&conn)?;

    // 5. Bootstrap import if DB is empty
    if bootstrap::is_db_empty(&conn)? {
        if std::path::Path::new(&config.bootstrap.config_file).exists() {
            tracing::info!(
                "DB is empty, importing bootstrap config from {}",
                config.bootstrap.config_file
            );
            match bootstrap::import_from_yaml(&conn, &config.bootstrap.config_file) {
                Ok(()) => tracing::info!("bootstrap import complete"),
                Err(e) => tracing::error!("bootstrap import failed: {}", e),
            }
        } else {
            tracing::warn!(
                "DB is empty but bootstrap file '{}' not found — \
                 server will start with no models/channels configured",
                config.bootstrap.config_file
            );
        }
    }

    // 6. Wrap connection in Arc<Mutex> for shared access
    let db: state::SharedDb = Arc::new(Mutex::new(conn));

    // 7. Build shared HTTP client (connection-pooled, reused by all upstream requests)
    let http_client = reqwest::Client::builder()
        .pool_max_idle_per_host(100)
        .pool_idle_timeout(std::time::Duration::from_secs(90))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .build()
        .expect("failed to build shared HTTP client");

    // 8. Build AppState
    let storage = Arc::new(StorageAdapter::new(db.clone()));
    let normalizer = Arc::new(Normalizer::new());
    let cache = Arc::new(ConfigCache::new(normalizer.clone()));
    let health = Arc::new(HealthManager::with_db(db.clone()));
    health.load_disabled_from_db().await;
    // Load small-model quota snapshots from DB so is_small_model_available
    // reflects persisted quota state immediately on startup.
    health.load_small_model_quota_from_db().await;
    let upstream_timeout = std::time::Duration::from_secs(config.server.upstream_timeout_secs);
    let streaming_timeout = std::time::Duration::from_secs(config.server.streaming_timeout_secs);
    let executor = Arc::new(Executor::new(
        health.clone(),
        cache.clone(),
        http_client.clone(),
        upstream_timeout,
        streaming_timeout,
    ));

    let session_store = new_session_store();

    let state = AppState {
        executor,
        cache,
        health,
        normalizer,
        storage,
        db,
        config: Arc::new(config),
        session_store,
        active_streams: Arc::new(AtomicUsize::new(0)),
        http_client,
    };

    // 9. Spawn background health-recovery loop
    //
    // Runs every 10s (was 30s) — `check_recoveries` is no longer called
    // per-request (see `select_keys` in executor.rs), so the background
    // loop is now the only driver of cooldown recovery + small-model quota
    // window rollover. A shorter interval reduces the lag on
    // `consecutive_failures` resets (which only affect backoff window
    // length, not availability — `is_available` checks `cooldown_until`
    // inline and is independent of this loop).
    let health_clone = state.health.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            health_clone.check_recoveries().await;
        }
    });

    // 9b. Spawn background quota-reset task
    let reset_db = state.db.clone();
    tokio::spawn(async move {
        let mut daily_interval = tokio::time::interval(tokio::time::Duration::from_secs(86400));
        let mut monthly_interval = tokio::time::interval(tokio::time::Duration::from_secs(86400 * 30));

        // Skip the first immediate tick for both intervals.
        daily_interval.tick().await;
        monthly_interval.tick().await;

        loop {
            tokio::select! {
                _ = daily_interval.tick() => {
                    let conn = reset_db.lock().await;
                    let key_repo = chennix_storage::keys::KeyRepo::new(&conn);
                    match key_repo.reset_daily_quota() {
                        Ok(count) => tracing::info!(count, "Reset daily quota for keys"),
                        Err(e) => tracing::warn!("Failed to reset daily quota: {}", e),
                    }
                }
                _ = monthly_interval.tick() => {
                    let conn = reset_db.lock().await;
                    let key_repo = chennix_storage::keys::KeyRepo::new(&conn);
                    match key_repo.reset_monthly_quota() {
                        Ok(count) => tracing::info!(count, "Reset monthly quota for keys"),
                        Err(e) => tracing::warn!("Failed to reset monthly quota: {}", e),
                    }
                }
            }
        }
    });

    // 10. Build router
    let addr = format!(
        "{}:{}",
        state.config.server.host, state.config.server.port
    );
    let tls_enabled = state.config.server.tls.enabled;
    let active_streams = state.active_streams.clone();
    let app = build_router(state);

    // 11. Start server
    tracing::info!("server listening on {}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await?;

    if tls_enabled {
        // TLS support — requires rustls or similar. For MVP we log a warning
        // and fall back to plain HTTP; a production deployment should front
        // the server with a TLS-terminating reverse proxy.
        tracing::warn!(
            "TLS is enabled in config but built-in TLS support is not yet \
             implemented. Starting in plain HTTP mode. Use a reverse proxy \
             (nginx/caddy) for TLS termination."
        );
    }

    // 12. Graceful shutdown — stop accepting new connections on Ctrl+C
    // (and SIGTERM on Unix), then wait for in-flight streaming billing
    // tasks to settle.
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm =
            signal(SignalKind::terminate()).expect("Failed to install SIGTERM handler");
        let shutdown = async {
            tokio::select! {
                _ = signal::ctrl_c() => {
                    tracing::info!("Received SIGINT, gracefully stopping...");
                }
                _ = sigterm.recv() => {
                    tracing::info!("Received SIGTERM, gracefully stopping...");
                }
            }
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;
    }
    #[cfg(not(unix))]
    {
        let shutdown = async {
            signal::ctrl_c()
                .await
                .expect("Failed to listen for Ctrl+C");
            tracing::info!("Received shutdown signal, gracefully stopping...");
        };
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown)
            .await?;
    }

    // Wait for in-flight streaming tasks (billing settlement) to complete,
    // so pre-charged quota is properly refunded/charged before exit.
    let remaining = active_streams.load(Ordering::SeqCst);
    if remaining > 0 {
        tracing::info!(
            "Waiting for {} in-flight streaming task(s) to settle...",
            remaining
        );
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            if active_streams.load(Ordering::SeqCst) == 0 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    "Timeout waiting for streaming tasks, {} still active \
                     — forcing shutdown",
                    active_streams.load(Ordering::SeqCst)
                );
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }
    tracing::info!("Shutdown complete");

    Ok(())
}

/// Build the axum router with all routes + middleware.
///
/// The auth middleware is only applied to the `/v1/*` proxy routes.
/// The `/health` endpoint is public (no auth) so it can be used by
/// load balancers / orchestrators for liveness checks.
fn build_router(state: AppState) -> Router {
    let admin_routes = admin_router(state.clone());

    let authed_routes = Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/messages", post(claude_messages))
        .route("/v1/models", get(list_models))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            token_auth_middleware,
        ));

    let public_routes = Router::new().route("/health", get(health_check));

    Router::new()
        .merge(authed_routes)
        .merge(admin_routes)
        .merge(static_files::web_routes())
        .merge(public_routes)
        .layer(CorsLayer::permissive())
        .layer(request_log_layer())
        .with_state(state)
}

/// Simple health-check handler.
async fn health_check() -> &'static str {
    "ok"
}

/// Initialize the tracing subscriber with the given log level.
fn init_tracing(level: &str) {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));

    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();
}
