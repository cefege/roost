//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "signal_kind.rs"]
mod signal_kind;
