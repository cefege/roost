//! The terminal half of one Sync v2 socket: the per-session lanes that carry
//! cell baselines, view-states and deltas to one viewer.
//!
//! Owned by the Sync session. Three files because three questions are asked of
//! it and none of them is answered by the other two: `lane` decides what
//! INBOUND terminal material joins a session, `ready_ring` decides what goes
//! OUT of one lane next, and `snapshot` is the seam to the terminal screen hub
//! that owns the canonical full every viewer reads.
//!
//! All three are methods on [`SyncV2Session`], not free functions over a
//! borrowed record. The state they share -- a charge held by exactly one of
//! {a domain queue, a cursor's materialization, a cursor's delta tail, a
//! lane's pending states} -- is an invariant of that one record, and splitting
//! it would turn the invariant into a protocol between two owners.

pub mod cursor;
pub mod delivery;
pub mod lane;
pub mod ready_ring;
pub mod snapshot;

pub use cursor::{SnapshotCursor, TerminalLane};
pub use lane::TerminalDeltaOutcome;
pub use snapshot::{
    CellGridSnapshotPlanError, NoTerminalSnapshotHub, ProtocolCellSnapshot, TerminalSnapshotCursor,
    TerminalSnapshotHub, TerminalSnapshotSource,
};
