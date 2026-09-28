//! The registry surface a stream-owning host needs beyond the membership
//! machine: one record by key, the session-wide view broadcast, and a socket's
//! per-session allowance.
//!
//! Ported from `packages/protocol/src/terminal-view/terminal-view-registry.ts`
//! (`broadcast`, and the `allowsSession` predicate `registerSocket` takes). A
//! child of `registry` so it reads the maps without widening their visibility;
//! the worker's `crates/roost-worker/src/terminal_view` owner is the caller.

use roost_proto::TerminalViewStatus;

use crate::wire::SessionId;

use super::super::record::ViewRecord;
use super::super::sink::PendingReply;
use super::ViewRegistry;

impl ViewRegistry {
    /// One record by its composite [`super::super::record::view_key`].
    ///
    /// A host that mints streams reads this BEFORE handing a command to the
    /// machine: v2's admit, reclaim and same- or new-revision update paths each
    /// finish differently once the geometry is recomputed, and the outcome
    /// alone does not say which path ran.
    #[must_use]
    pub fn view(&self, key: &str) -> Option<&ViewRecord> {
        self.views.get(key)
    }

    /// One `View` answer per live record of the session, in the session
    /// index's order. Parked records are skipped: their socket is gone.
    ///
    /// v2's `broadcast`: a stream owner tells every live view about a new
    /// stream, or that the stream is unavailable, through this.
    #[must_use]
    pub fn broadcast(
        &self,
        session_id: &SessionId,
        status: TerminalViewStatus,
        reason: &str,
    ) -> Vec<PendingReply> {
        self.session_views
            .get(session_id)
            .into_iter()
            .flatten()
            .filter_map(|key| self.views.get(key))
            .filter(|record| !record.parked)
            .map(|record| PendingReply::View {
                socket_id: record.socket_id.clone(),
                view_id: record.view_id.clone(),
                session_id: record.intent.session_id.clone(),
                revision: record.revision,
                status,
                reason: reason.to_owned(),
            })
            .collect()
    }

    /// Admit or withdraw one session for one registered socket.
    ///
    /// v2 registers a socket with an `allowsSession(sessionId)` PREDICATE: a
    /// coordinator-relayed socket allows every session the coordinator already
    /// authorized, and a local socket follows its grant's live scope. The
    /// machine only ever asks about the session a command names, so a host that
    /// sets that one answer right before the command reproduces the predicate.
    pub fn set_session_allowed(&mut self, socket_id: &str, session_id: &str, allowed: bool) {
        let Some(socket) = self.sockets.get_mut(socket_id) else {
            return;
        };
        if allowed {
            socket.allowed_sessions.insert(session_id.to_owned());
        } else {
            socket.allowed_sessions.remove(session_id);
        }
    }
}
