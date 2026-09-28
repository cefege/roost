//! Where a replica stands, as a resync request names it: the stream it expects
//! and how far along that stream its canonical grid is.
//!
//! Read by `handle_sweep::request_repair_if_due` and the route-loss path in
//! `handle_input`, which put it on `SyncCommand::TerminalResync`. Ported from
//! v2's `sendTerminalResyncCommand` (`apps/web/src/store/terminal-stream-repair.ts:221-235`).

use crate::terminal::session::TerminalSession;

/// The stream position one resync request carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResyncPosition {
    /// The stream the replica is fenced to.
    pub stream_id: String,
    /// The canonical grid's epoch, or empty with no canonical.
    pub grid_epoch: String,
    /// The canonical grid's sequence, or `0` with no canonical.
    pub seq: u64,
}

impl TerminalSession {
    /// The position a resync should name, or `None` when the replica expects no
    /// stream yet. v2 sends NO resync then: a baseline request with no stream to
    /// be a baseline of is a request the authority cannot place, and the view
    /// acceptance that installs the expectation brings a fresh full anyway.
    pub fn resync_position(&self) -> Option<ResyncPosition> {
        let stream_id = self.expected_stream_id()?.to_string();
        let canonical = self.canonical();
        Some(ResyncPosition {
            stream_id,
            grid_epoch: canonical.map(|frame| frame.grid_epoch.clone()).unwrap_or_default(),
            seq: canonical.map_or(0, |frame| frame.seq),
        })
    }
}
