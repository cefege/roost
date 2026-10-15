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
#[path = "loop_mode.rs"]
mod loop_mode;
#[path = "model_presets.rs"]
mod model_presets;
#[path = "model_scope.rs"]
mod model_scope;
#[path = "model_switch.rs"]
mod model_switch;
#[path = "plan_and_tasks.rs"]
mod plan_and_tasks;
#[path = "quality_tools.rs"]
mod quality_tools;
#[path = "rewind_fork.rs"]
mod rewind_fork;
#[path = "roles.rs"]
mod roles;
#[path = "web_tools.rs"]
mod web_tools;
