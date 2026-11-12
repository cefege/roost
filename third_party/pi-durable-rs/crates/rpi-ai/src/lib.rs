//! `rpi-ai` — port of `@earendil-works/pi-ai` @ pi 0.82.1 (2efa728).
//!
//! Unified multi-protocol LLM access: core types, streaming events, tool schema
//! validation, usage/cost, model catalog, auth, cross-provider message
//! transforms, retry and overflow detection.
//!
//! Pure library crate: no bin target (the pi-ai package-level CLI is explicitly
//! out of scope, ADR-0003 §4). Depends on no other internal crate
//! (coding-standards §2.2).

pub mod api;
pub mod auth;
pub mod azure_provider_rename;
pub mod error;
pub mod generated;
pub mod images;
pub mod models;
pub mod models_json;
pub mod models_store;
pub mod providers;
pub mod session_resources;
pub mod types;
pub mod utils;

pub use error::AiError;
pub use types::*;
