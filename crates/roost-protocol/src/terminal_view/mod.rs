//! Terminal view membership shared by every view owner: the socket registry,
//! leases, park grace, retained claims and the one geometry predicate. Ported
//! from `packages/protocol/src/terminal-view/*`; used by the coordinator's
//! `TerminalViewHub` and the worker's `terminal_view` owner, which is why it is
//! here and not in either — one registry, two hosts, never two registries.

mod admit;
mod commands;
mod machine;
mod record;
mod registry;
mod sink;
mod tombstone;

pub use machine::Machine;
pub use record::{
    GeometrySet, SESSION_VIEW_CAP, TombstoneStore, ViewInput, ViewIntent, ViewRecord, ViewStats,
    active_fingerprints, geometry_set, intent_of, intents_equal, project_inputs, project_viewers,
    validate_view_command, view_constrains, view_key,
};
pub use registry::{MembershipOutcome, SocketRecord, SocketRegistration, ViewRegistry};
pub use sink::{
    NoTerminalViewSink, PendingReply, SinkCall, TerminalViewSink, VIEW_REASON_MAX_BYTES,
    truncate_view_reason, view_state_frame,
};
pub use tombstone::VIEWER_TOMBSTONE_CAP;
