//! The worker's terminal sessions: the record every slice keys on, the ring it
//! retains bytes in, the history and agent evidence it carries, and the traits
//! that deliver into it. `runtime::serve` owns the manager that holds them;
//! `session::spawn`, `session::lifecycle` and `session::emit` own its
//! transitions. Depends on `roost_term` for the core, `event_store` for the
//! durable claim, and `crate::shell_spec` for the launch contract — and nothing
//! here depends on any of them back.
//!
//! The declarations below are the lead's, not the slices': `resume`, `respawn`,
//! `resize` and `binding` all belong to the lifecycle slice, and a `pub mod`
//! for a file that is not on disk yet is a hard compile error for every slice
//! sharing this crate. They are declared together so a file is written before
//! it is named, and never named twice.

pub mod agent_osc;
pub mod binding;
pub mod cell_scheduler;
pub mod cell_sink;
pub mod control_lanes;
pub mod emit;
pub mod history;
pub mod ids;
pub mod keeper_admission;
pub mod lifecycle;
pub mod raw_metadata;
pub mod respawn;
pub mod resize;
pub mod resume;
pub mod retained_grid;
pub mod ring;
pub mod scrollback;
pub mod sinks;
pub mod snapshot_cursor;
pub mod spawn;
pub mod stream_scan;
pub mod types;
