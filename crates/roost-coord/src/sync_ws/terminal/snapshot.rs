//! The seam between the Sync session and the terminal screen hub: a canonical
//! full a socket walks one part at a time, and the one call the session makes
//! back into the hub.
//!
//! Owned by the Sync session's terminal half. The hub is a different slice
//! (`apps/coord/src/terminal/screen` in v2), so the dependency is a trait here
//! and a parameter at every call site rather than an import into a module this
//! crate does not own.
//!
//! WHY A TRAIT AND NOT THE HUB'S TYPE. The session must be testable without a
//! screen replica, a worker, or a real 80x24 grid, and a 256 KiB frame per
//! assertion is not a test. The trait has two implementations in this tree: the
//! protocol-backed one below, which plans and materialises through
//! `roost-protocol` and therefore cannot drift from the planner the worker
//! side uses, and whatever the terminal-view hub's owner writes.
//!
//! `release()` from v2 is gone, and that is the point. v2's cursor held a lease
//! that had to be dropped by hand, and `materialize` after a release threw
//! (`terminal-screen-frames.ts:37-40`). Here a cursor is an `Arc` and dropping
//! it releases the source, so a use-after-release is a compile error rather
//! than a runtime one and there is no flag to keep honest.

use std::sync::Arc;

use roost_protocol::cell::frame_chunks::CellGridSnapshotSource;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;

use crate::sync_ws::retained_frame::SharedCellFrame;

/// One recipient's walk over a canonical terminal full.
pub trait TerminalSnapshotCursor: Send + Sync {
    /// How many parts this walk has. Always at least one: a source with no
    /// parts is refused by the hub rather than admitted here.
    fn part_count(&self) -> u32;

    /// One part, as this recipient's own snapshot.
    ///
    /// `None` means the index is outside the plan. That is a caller bug, not a
    /// client-visible fault: the session only ever asks for `index` while
    /// `index < part_count`, and a source that answers `None` there makes the
    /// lane rebaseline rather than send a hole.
    fn materialize(&self, part_index: u32) -> Option<SharedCellFrame>;
}

/// A canonical terminal full that any number of sockets may walk at their own
/// pace. One source, one grid, N cursors.
pub trait TerminalSnapshotSource: Send + Sync {
    /// One recipient's cursor, stamped with its own snapshot id.
    ///
    /// `snapshot_id` is supplied by the caller rather than minted here: the
    /// snapshot id namespace belongs to the hub that owns the replica, and
    /// every cursor of one source carries a DIFFERENT id, which is what lets two
    /// viewers stream the same baseline without their parts merging.
    fn create_cursor(&self, snapshot_id: &str) -> Option<Arc<dyn TerminalSnapshotCursor>>;
}

/// The terminal screen hub's one call, as the Sync session needs it.
pub trait TerminalSnapshotHub {
    /// Ask the hub for a canonical full for one session of one socket.
    ///
    /// RETURN `true` ONLY when a fresh source is now installed for that
    /// socket's session, because `false` is what tells the session the socket
    /// must wait for a snapshot rather than retry forever. A hub with nothing
    /// resident answers `false`, which is the v2 default
    /// (`sync-ws-v2-terminal.ts:47-49`).
    fn request_rebaseline(&mut self, socket_id: &str, session_id: &str) -> bool;
}

/// A hub that never has a source, so a session without a screen replica
/// rebaselines into a request that goes nowhere. The v2 default, kept as a
/// value so a caller does not have to write an empty impl.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTerminalSnapshotHub;

impl TerminalSnapshotHub for NoTerminalSnapshotHub {
    fn request_rebaseline(&mut self, _socket_id: &str, _session_id: &str) -> bool {
        false
    }
}

/// A canonical full planned by `roost-protocol`, shared by every cursor.
#[derive(Debug, Clone)]
pub struct ProtocolCellSnapshot {
    source: CellGridSnapshotSource,
}

impl ProtocolCellSnapshot {
    /// Plan `frame` into a source, refusing anything that is not a complete
    /// full before a byte of it is shared.
    pub fn plan(frame: &roost_proto::PbCellGridFrame) -> Result<Self, CellGridSnapshotPlanError> {
        let source = roost_protocol::cell::frame_chunks::create_cell_grid_snapshot_source(
            frame,
            true,
        )
        .map_err(|error| CellGridSnapshotPlanError {
            reason: error.to_string(),
        })?;
        Ok(Self { source })
    }

    /// How many parts every cursor of this source walks.
    #[must_use]
    pub fn part_count(&self) -> u32 {
        self.source.part_count()
    }
}

/// A source that is not a complete, bounded full.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cell snapshot cannot be planned for Sync fan-out: {reason}")]
pub struct CellGridSnapshotPlanError {
    /// Why the planner refused, in the protocol's own words.
    pub reason: String,
}

impl TerminalSnapshotSource for ProtocolCellSnapshot {
    fn create_cursor(&self, snapshot_id: &str) -> Option<Arc<dyn TerminalSnapshotCursor>> {
        // Validate the id once, here, so a caller that passes a non-snapshot id
        // is refused before any part is materialised rather than on part zero.
        self.source.create_cursor(snapshot_id).ok()?;
        Some(Arc::new(ProtocolCellSnapshotCursor {
            source: self.clone(),
            snapshot_id: snapshot_id.to_owned(),
        }))
    }
}

/// One recipient's cursor: the shared plan plus this recipient's snapshot id.
#[derive(Debug)]
struct ProtocolCellSnapshotCursor {
    source: ProtocolCellSnapshot,
    snapshot_id: String,
}

impl TerminalSnapshotCursor for ProtocolCellSnapshotCursor {
    fn part_count(&self) -> u32 {
        self.source.part_count()
    }

    fn materialize(&self, part_index: u32) -> Option<SharedCellFrame> {
        let cursor = self.source.source.create_cursor(&self.snapshot_id).ok()?;
        match cursor.materialize(part_index).ok()? {
            CellGridSnapshotPart::Frame(frame) => Some(SharedCellFrame::Full(frame)),
            CellGridSnapshotPart::Chunk(chunk) => Some(SharedCellFrame::Chunk(chunk)),
        }
    }
}
