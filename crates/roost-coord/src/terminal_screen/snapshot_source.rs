//! A resident canonical full as a snapshot source every watching socket walks,
//! each cursor holding a residency lease on the exact version it reads.
//!
//! Ports `apps/coord/src/terminal/screen/terminal-screen-frames.ts`
//! (`terminalSnapshotSource`, `cellGridEnvelope`). The screen hub plans one
//! source per cache version; the Sync lane owns the cursors.

use std::sync::{Arc, Weak};

use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{FirehoseFrame, PbCellGridFrame};
use roost_protocol::cell::cell_frame_to_proto;
use roost_protocol::cell::frame_chunk_validation::CELL_GRID_COORD_FANOUT_STAMP_MAX;
use roost_protocol::wire::SessionId;

use crate::sync_ws::retained_frame::SharedCellFrame;
use crate::sync_ws::terminal::snapshot::{
    ProtocolCellSnapshot, TerminalSnapshotCursor, TerminalSnapshotSource,
};
use crate::terminal_screen::replica::ScreenHub;
use crate::terminal_screen::residency::ResidentCache;

/// One delta, wrapped as the Sync frame a socket queues.
#[must_use]
pub fn cell_grid_envelope(frame: PbCellGridFrame) -> FirehoseFrame {
    FirehoseFrame {
        frame: Some(Frame::CellGrid(Box::new(frame))),
        ..FirehoseFrame::default()
    }
}

/// One immutable cache version, planned once and walked by any number of
/// sockets.
pub struct ResidentSnapshotSource {
    hub: Weak<ScreenHub>,
    session_id: SessionId,
    generation: u64,
    plan: ProtocolCellSnapshot,
}

impl std::fmt::Debug for ResidentSnapshotSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ResidentSnapshotSource")
            .field("session_id", &self.session_id)
            .field("generation", &self.generation)
            .field("parts", &self.plan.part_count())
            .finish()
    }
}

impl ResidentSnapshotSource {
    /// Plan `cache` for fan-out. The plan reserves the widest fan-out stamp a
    /// recipient's egress can write, so no part outgrows its limit once
    /// stamped (`reserveFanoutStamp`, `terminal-screen-frames.ts:64-67`).
    pub(crate) fn plan(
        hub: &Arc<ScreenHub>,
        session_id: &SessionId,
        cache: &ResidentCache,
    ) -> Result<Self, String> {
        let mut proto = cell_frame_to_proto(&cache.frame, session_id.as_str())
            .map_err(|error| error.to_string())?;
        proto.coord_recv_ms = cache.coord_recv_ms;
        proto.coord_fanout_ms = CELL_GRID_COORD_FANOUT_STAMP_MAX;
        let plan = ProtocolCellSnapshot::plan(&proto).map_err(|error| error.reason)?;
        Ok(Self {
            hub: Arc::downgrade(hub),
            session_id: session_id.clone(),
            generation: cache.generation,
            plan,
        })
    }

    /// The cache version this source was planned from.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl TerminalSnapshotSource for ResidentSnapshotSource {
    fn create_cursor(&self, snapshot_id: &str) -> Option<Arc<dyn TerminalSnapshotCursor>> {
        let hub = self.hub.upgrade()?;
        // A cache superseded before the socket admitted it is refused here:
        // only a leased cursor may read a version, so a slow socket cannot
        // keep an uncharged grid alive.
        if !hub.acquire_source_lease(&self.session_id, self.generation) {
            tracing::debug!(
                session_id = %self.session_id,
                generation = self.generation,
                "a terminal snapshot source is no longer resident"
            );
            return None;
        }
        let Some(inner) = self.plan.create_cursor(snapshot_id) else {
            hub.release_source_lease(&self.session_id, self.generation);
            return None;
        };
        Some(Arc::new(LeasedCursor {
            inner,
            hub: Arc::downgrade(&hub),
            session_id: self.session_id.clone(),
            generation: self.generation,
        }))
    }
}

/// A cursor that gives its version's lease back when the socket drops it.
struct LeasedCursor {
    inner: Arc<dyn TerminalSnapshotCursor>,
    hub: Weak<ScreenHub>,
    session_id: SessionId,
    generation: u64,
}

impl TerminalSnapshotCursor for LeasedCursor {
    fn part_count(&self) -> u32 {
        self.inner.part_count()
    }

    fn materialize(&self, part_index: u32) -> Option<SharedCellFrame> {
        self.inner.materialize(part_index)
    }
}

impl Drop for LeasedCursor {
    fn drop(&mut self) {
        if let Some(hub) = self.hub.upgrade() {
            hub.release_source_lease(&self.session_id, self.generation);
        }
    }
}
