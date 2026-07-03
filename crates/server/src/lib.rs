//! Library entry point for chennix-server.
//!
//! This file exists solely to expose the server's internal modules
//! (config, middleware, pipeline, routes, state) to integration tests
//! in `tests/`. The binary target (`src/main.rs`) declares the same
//! modules privately; both targets compile the same source files
//! independently.
//!
//! No business logic lives here — every module is `pub mod` re-export.

pub mod admin;
pub mod config;
pub mod middleware;
pub mod pipeline;
pub mod routes;
pub mod state;
pub mod static_files;
