//! One session's terminal lane: the baseline cursor, the buffered deltas, and
//! the charges they hold.
//!
//! Owned by the Sync session. This file is the DATA half of the terminal lane
//! and `lane.rs` is its INGRESS half, and the split is the 400-line cap rather
//! than a boundary: the two halves share one invariant -- a charge is held by
//! exactly one of {a domain queue, a cursor's materialisation, a cursor's delta
//! tail, a lane's pending states} -- and that invariant is why the release
//! methods live beside the fields they clear.
//!
//! WHY A CHARGE IS HELD BY EXACTLY ONE OWNER. In v2 the cursor's `materialized`
//! field aliased the same charge object the domain queue held, and the first of
//! the two to release it won. That is safe in JS and unexpressible in Rust
//! without interior mutability, so the port gives the charge to the QUEUE at
//! enqueue time and leaves `materialized` empty from that moment. A frame is
//! charged once and released once, and `materialized` means exactly "a part is
//! charged and not yet queued".

use std::sync::Arc;

use crate::sync_ws::retained_frame::RetainedFrame;
use crate::sync_ws::session::SyncV2Session;

use super::snapshot::TerminalSnapshotCursor;

/// One session's walk over a canonical full, plus the deltas that arrived
/// behind it.
pub struct SnapshotCursor {
    /// The stream this cursor belongs to. A cursor from an older stream is
    /// never advanced by a delivery from the newer one.
    pub stream_id: String,
    pub(in crate::sync_ws) source: Option<Arc<dyn TerminalSnapshotCursor>>,
    /// The next part to send.
    pub index: u32,
    /// Every part of one chunked snapshot keeps identical timing metadata, so
    /// the stamp is taken at the first part and reused for the rest.
    pub fanout_ms: Option<u64>,
    /// Whether the part at `index` is in flight.
    pub queued: bool,
    /// The one part charged to this cursor and not yet queued.
    pub(in crate::sync_ws) materialized: Option<RetainedFrame>,
    /// Deltas that arrived while the baseline was still streaming.
    pub delta_tail: Vec<RetainedFrame>,
    /// The tail's charged bytes.
    pub delta_bytes: u64,
}

/// The cursor without its source handle, which is a trait object and therefore
/// not `Debug`. Everything a diagnostic needs to name a lane's position is here.
impl std::fmt::Debug for SnapshotCursor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SnapshotCursor")
            .field("stream_id", &self.stream_id)
            .field("index", &self.index)
            .field("part_count", &self.part_count())
            .field("queued", &self.queued)
            .field("fanout_ms", &self.fanout_ms)
            .field("delta_frames", &self.delta_tail.len())
            .field("delta_bytes", &self.delta_bytes)
            .finish()
    }
}

impl SnapshotCursor {
    /// A cursor for a stream with no baseline yet: deltas only.
    pub(in crate::sync_ws) fn of_stream(stream_id: &str) -> Self {
        Self {
            stream_id: stream_id.to_owned(),
            source: None,
            index: 0,
            fanout_ms: None,
            queued: false,
            materialized: None,
            delta_tail: Vec::new(),
            delta_bytes: 0,
        }
    }

    /// A cursor at the start of a baseline.
    pub(in crate::sync_ws) fn of_snapshot(
        stream_id: &str,
        source: Arc<dyn TerminalSnapshotCursor>,
    ) -> Self {
        Self {
            source: Some(source),
            ..Self::of_stream(stream_id)
        }
    }

    /// How many parts the baseline has, or zero when there is no baseline.
    pub(in crate::sync_ws) fn part_count(&self) -> u32 {
        self.source.as_ref().map_or(0, |source| source.part_count())
    }

    /// Whether a baseline is part-way through and must not be abandoned.
    ///
    /// Index zero counts as NOT part-way: nothing of it has gone out, so
    /// dropping it costs the client nothing, and a lane that abandoned every
    /// baseline before its first part would rebaseline in a loop.
    pub(in crate::sync_ws) fn is_mid_baseline(&self) -> bool {
        self.source.is_some() && self.index > 0 && self.index < self.part_count()
    }
}

/// One session's terminal lane.
#[derive(Debug)]
pub struct TerminalLane {
    /// The terminal stream this lane is carrying. A new stream id retires
    /// everything the old one had queued.
    pub stream_id: String,
    pub(in crate::sync_ws) cursor: Option<SnapshotCursor>,
    /// View-states waiting for this session's turn, oldest first.
    pub(in crate::sync_ws) pending_states: Vec<RetainedFrame>,
    /// Whether one of them is in flight. One at a time, so a state can never
    /// overtake the state before it.
    pub(in crate::sync_ws) state_queued: bool,
    /// Whether this lane is in the socket's ready ring.
    pub ready: bool,
    /// A scoped canonical full is owed once the current cursor can release.
    pub rebaseline_pending: bool,
    /// The next baseline part may pass other sessions' queued deltas once.
    pub attach_priority_pending: bool,
}

impl TerminalLane {
    pub(in crate::sync_ws) fn new(stream_id: &str, attach_priority_pending: bool) -> Self {
        Self {
            stream_id: stream_id.to_owned(),
            cursor: None,
            pending_states: Vec::new(),
            state_queued: false,
            ready: false,
            rebaseline_pending: false,
            attach_priority_pending,
        }
    }

    /// Release every charge this lane holds.
    pub(in crate::sync_ws) fn release_all(&mut self, session: &mut SyncV2Session) {
        let states = std::mem::take(&mut self.pending_states);
        for mut state in states {
            session.release_charge(&mut state);
        }
        self.state_queued = false;
        self.ready = false;
        self.rebaseline_pending = false;
        self.release_cursor(session);
    }

    /// Release the baseline cursor and everything it holds.
    pub(in crate::sync_ws) fn release_cursor(&mut self, session: &mut SyncV2Session) {
        let Some(mut cursor) = self.cursor.take() else {
            return;
        };
        if let Some(mut materialized) = cursor.materialized.take() {
            session.release_charge(&mut materialized);
        }
        cursor.release_delta_tail(session);
    }

    /// Release the buffered deltas, keeping the baseline cursor.
    pub(in crate::sync_ws) fn release_delta_tail(&mut self, session: &mut SyncV2Session) {
        if let Some(cursor) = self.cursor.as_mut() {
            cursor.release_delta_tail(session);
        }
    }
}

impl SnapshotCursor {
    fn release_delta_tail(&mut self, session: &mut SyncV2Session) {
        let deltas = std::mem::take(&mut self.delta_tail);
        for mut delta in deltas {
            session.release_charge(&mut delta);
        }
        self.delta_bytes = 0;
    }
}
