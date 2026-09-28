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
pub mod binding_close;
mod binding_staging;
pub mod cell_gates;
pub mod cell_scheduler;
pub mod cell_sink;
pub mod cell_sink_lifecycle;
pub mod channel_creation_gate;
pub mod closed_hooks;
pub mod control_lanes;
pub mod core_reprove;
pub mod cwd_events;
pub mod durable_delivery;
pub mod durable_sink;
pub mod emit;
pub mod emit_frame;
pub mod emit_ingest;
pub mod emit_streams;
pub mod folder_hooks;
pub mod git_ports;
pub mod history;
pub mod ids;
pub mod input_write;
pub mod journal_sink;
pub mod keeper_admission;
pub mod keeper_channels;
pub mod keeper_health;
pub mod lifecycle;
pub mod lifecycle_commands;
pub mod query_reply;
pub mod raw_metadata;
pub mod replay_align;
pub mod resize;
pub mod resize_pin;
pub mod respawn;
pub mod respawn_replace;
pub mod resume;
mod resume_core;
pub mod retained_grid;
pub mod ring;
pub mod scrollback;
pub mod sinks;
pub mod snapshot_cursor;
pub mod snapshot_cursor_drain;
pub mod spawn;
pub mod stray_reap;
pub mod stream_scan;
pub mod sync_output;
pub mod table;
pub mod terminal_changed;
pub mod terminal_control;
pub mod terminal_metadata;
pub mod terminal_state;
pub mod terminal_stream_owner;
pub mod terminal_txn;
pub mod types;
pub mod unhandled_seq;
