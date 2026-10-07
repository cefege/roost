//! The store's per-session terminal replicas: create on first use, look up,
//! and ask whether one can be painted.
//!
//! An inherent `impl Store` split out of `store.rs` to keep that file under the
//! size cap; the replicas are still the `Store::terminal` field.

use crate::store::Store;
use crate::terminal::session::TerminalSession;

impl Store {
    /// A session's replica, creating it on first use.
    ///
    /// Created lazily because a session with no pane has no replica to keep, and
    /// a pre-created one per session in a large account is a per-session grid the
    /// client never needed to hold.
    pub fn terminal_mut(&mut self, session_id: &str, worker_fp: &str) -> &mut TerminalSession {
        self.terminal
            .entry(session_id.to_string())
            .or_insert_with(|| TerminalSession::new(session_id, worker_fp))
    }

    /// A session's replica, if one exists.
    pub fn terminal(&self, session_id: &str) -> Option<&TerminalSession> {
        self.terminal.get(session_id)
    }

    /// A session's replica, mutable, if one exists.
    pub fn terminal_mut_if_present(&mut self, session_id: &str) -> Option<&mut TerminalSession> {
        self.terminal.get_mut(session_id)
    }

    /// Whether a session's replica is holding a complete baseline.
    pub fn is_paintable(&self, session_id: &str) -> bool {
        self.terminal
            .get(session_id)
            .is_some_and(|session| session.baseline_ready())
    }
}
