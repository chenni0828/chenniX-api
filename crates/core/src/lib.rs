//! chennix-core: routing, health, billing, execution.
//!
//! This crate ties together the adaptor (upstream protocol), translator
//! (cross-format request/response conversion), storage (config + usage),
//! and an in-process runtime (health/cooldown/quota) to implement the
//! proxy's request execution pipeline.

pub mod billing;
pub mod billing_expr;
pub mod cache;
pub mod executor;
pub mod health;
pub mod normalizer;
pub mod router;
pub mod tracker;
