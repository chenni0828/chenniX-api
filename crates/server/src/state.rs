//! Shared application state — injected into every request handler via
//! `axum::extract::State`.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::sync::Mutex;

use rusqlite::Connection;

use chennix_core::cache::ConfigCache;
use chennix_core::executor::Executor;
use chennix_core::health::HealthManager;
use chennix_core::normalizer::Normalizer;

use crate::admin::auth::SessionStore;
use crate::config::AppConfig;
use crate::pipeline::StorageAdapter;

/// Type alias for the shared SQLite connection.
pub type SharedDb = Arc<Mutex<Connection>>;

#[derive(Clone)]
pub struct AppState {
    pub executor: Arc<Executor>,
    pub cache: Arc<ConfigCache>,
    pub health: Arc<HealthManager>,
    pub normalizer: Arc<Normalizer>,
    pub storage: Arc<StorageAdapter>,
    pub db: SharedDb,
    pub config: Arc<AppConfig>,
    /// In-memory session store for admin panel authentication.
    pub session_store: SessionStore,
    /// Tracks the number of in-flight streaming tasks (billing settlement
    /// spawned via `tokio::spawn`). Used during graceful shutdown to wait
    /// for all streaming billing to complete before the process exits.
    pub active_streams: Arc<AtomicUsize>,
    /// 共享的 HTTP Client（连接池复用）。所有上游请求都通过这个 Client 发送。
    pub http_client: reqwest::Client,
}

impl AppState {
    /// The storage adapter (also accessible as `state.storage` field directly).
    #[allow(dead_code)]
    pub fn storage(&self) -> &Arc<StorageAdapter> {
        &self.storage
    }
}
