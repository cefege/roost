//! Which devices are watching a session, as the push dispatch asks it: the
//! coordinator's `TerminalViewHub` answering [`ActiveTerminalViewers`].
//!
//! Owned by the push domain; `serve.rs` supplies the view hub through this impl
//! to suppress notifications to devices already looking at the terminal.
//!
//! THE OWNER ROW WINS, EVEN WHEN IT IS EMPTY. v2 answers
//! `ownerTerminalViewerFingerprints(sessionId) ?? hub.activeViewerFingerprints`:
//! a session whose owner-mode worker published ANY membership row (an empty
//! one included) is answered from that row alone, and only a session with no
//! row falls back to the coordinator's own registry. The owner row lists every
//! viewer the worker admitted, parked ones included, because a device that has
//! the terminal open in a background tab has still seen it.

use std::collections::BTreeSet;

use roost_protocol::wire::SessionId;

use crate::push::dispatch::ActiveTerminalViewers;
use crate::terminal_view::TerminalViewHub;

impl ActiveTerminalViewers for TerminalViewHub {
    fn active_viewer_fingerprints(&self, session_id: &SessionId) -> BTreeSet<String> {
        match self.owners().row(session_id) {
            Some(row) => row.viewer_fingerprints(),
            None => TerminalViewHub::active_viewer_fingerprints(self, session_id),
        }
    }
}
