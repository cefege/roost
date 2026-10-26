//! Every integration test of this crate, compiled as one binary. Generated and
//! checked by `cargo xtask lint`; run with `cargo nextest run`, which gives
//! each test its own process.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::duplicate_mod)]

#[path = "advisor.rs"]
mod advisor;
#[path = "agent_loop.rs"]
mod agent_loop;
#[path = "background_jobs.rs"]
mod background_jobs;
#[path = "durable_branch_state.rs"]
mod durable_branch_state;
#[path = "durable_code_mode.rs"]
mod durable_code_mode;
#[path = "durable_images.rs"]
mod durable_images;
#[path = "durable_publication.rs"]
mod durable_publication;
#[path = "durable_rewind.rs"]
mod durable_rewind;
#[path = "durable_yield.rs"]
mod durable_yield;
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
#[path = "peer_messages.rs"]
mod peer_messages;
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
