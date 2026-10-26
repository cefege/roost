//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "accounts.rs"]
mod accounts;
#[path = "provider_auth.rs"]
mod provider_auth;
#[path = "provider_cache.rs"]
mod provider_cache;
#[path = "provider_stream.rs"]
mod provider_stream;
#[path = "provider_support.rs"]
mod provider_support;
#[path = "web_search.rs"]
mod web_search;
