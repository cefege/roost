//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "advisor.rs"]
mod advisor;
#[path = "agent_loop.rs"]
mod agent_loop;
#[path = "find.rs"]
mod find;
#[path = "plan_and_tasks.rs"]
mod plan_and_tasks;
#[path = "roles.rs"]
mod roles;
