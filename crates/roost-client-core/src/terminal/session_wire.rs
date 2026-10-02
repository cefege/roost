//! The replica's diagnostic wire record: the last stream, grid epoch and
//! sequence that reached the canonical path, whatever their fate.
//!
//! Split out of `session` because it is read for a different reason than
//! admission is: `wire_received` is how a stuck session is told "the sender
//! thinks it is on X" apart from "the sender is on X and we are not accepting
//! it". Written by `session`'s admission and by `handle_sync::candidate`'s
//! promotion commit. Ports v2 `noteWireFrame` (`terminal-stream-replica.ts`).

use crate::terminal::session::TerminalSession;

impl TerminalSession {
    /// Record one frame or chunk part that reached this replica.
    pub(crate) fn note_wire(&mut self, stream_id: &str, grid_epoch: &str, seq: u64) {
        self.wire_stream_id = Some(stream_id.to_string());
        self.wire_grid_epoch = Some(grid_epoch.to_string());
        self.wire_seq = Some(seq);
    }

    /// Take over the wire record of the replica a promotion replaces.
    ///
    /// The record is what the CANONICAL path received (v2's `noteWireFrame` runs
    /// only in canonical dispatch); a candidate's staging frames are not part of
    /// it, so a route that won before any fallback frame arrived reads none.
    pub(crate) fn inherit_wire_record(&mut self, replaced: Option<&TerminalSession>) {
        self.wire_stream_id = replaced.and_then(|replica| replica.wire_stream_id.clone());
        self.wire_grid_epoch = replaced.and_then(|replica| replica.wire_grid_epoch.clone());
        self.wire_seq = replaced.and_then(|replica| replica.wire_seq);
    }
}
