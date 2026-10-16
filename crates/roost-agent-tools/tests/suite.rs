//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "background_jobs.rs"]
mod background_jobs;
#[path = "file_tools.rs"]
mod file_tools;
#[path = "hashline.rs"]
mod hashline;
#[path = "restore_points.rs"]
mod restore_points;
