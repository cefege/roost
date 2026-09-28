//! The screen replica's owner in a coordinator whose workers own their
//! terminal views: a lost baseline is repaired by the session's owning worker.
//!
//! Ports the owner-mode answers of the `TerminalScreenHub` callbacks v2 wires in
//! `terminal/view/terminal-view-stream-controller.ts:50-68` and
//! `terminal-view-stream-snapshot-request.ts:31-38` (`repairUnownedSession`).
//! `services.rs` installs it into the screen hub; the hub calls it.

use std::sync::{Arc, Weak};

use roost_protocol::wire::SessionId;

use crate::terminal_screen::hub_contract::ScreenReplicaSink;

use super::TerminalViewHub;

/// Snapshot repair through the view owner relay.
#[derive(Debug, Clone)]
pub struct OwnerScreenRepair {
    views: Weak<TerminalViewHub>,
}

impl OwnerScreenRepair {
    /// A repair sink over the process's view hub. Weak: the hub's sockets
    /// reach the screen replica, and a strong edge back would be a cycle.
    #[must_use]
    pub fn new(views: Weak<TerminalViewHub>) -> Self {
        Self { views }
    }
}

impl ScreenReplicaSink for OwnerScreenRepair {
    fn request_snapshot(&self, session_id: &SessionId, stream_id: &str) {
        if let Some(views) = self.views.upgrade() {
            views.repair_session(session_id, stream_id);
        }
    }

    /// The coordinator mints no stream for a session its worker owns, so there
    /// is nothing here to re-drive: the owner's next view state installs one.
    fn request_fresh_stream(&self, session_id: &SessionId, expected_stream_id: &str, reason: &str) {
        tracing::warn!(
            session_id = %session_id,
            expected_stream_id,
            reason,
            "a terminal screen repair timed out; the owning worker's next view state is the only fresh stream"
        );
    }

    /// Unavailability is the owner's to announce for a session it owns; the
    /// coordinator logs the replica's reason.
    fn unavailable(&self, session_id: &SessionId, reason: &str) {
        tracing::warn!(session_id = %session_id, reason, "a terminal screen became unavailable");
    }

    fn full_accepted(&self, session_id: &SessionId, stream_id: &str) {
        tracing::debug!(session_id = %session_id, stream_id, "a terminal baseline was accepted");
    }
}

impl TerminalViewHub {
    /// Ask the session's owning worker for a source full of `stream_id`.
    /// A session with no owner has nobody to ask, and says so.
    pub fn repair_session(&self, session_id: &SessionId, stream_id: &str) -> bool {
        let Some(owner) = self.owners.owner_for_session(session_id) else {
            tracing::warn!(
                %session_id,
                stream_id,
                "a terminal snapshot repair has no owning worker to ask"
            );
            return false;
        };
        self.relay.repair_session(&owner, session_id, stream_id)
    }
}

/// The process's repair sink, as the screen hub stores it.
#[must_use]
pub fn owner_screen_repair(views: &Arc<TerminalViewHub>) -> Arc<dyn ScreenReplicaSink> {
    Arc::new(OwnerScreenRepair::new(Arc::downgrade(views)))
}
