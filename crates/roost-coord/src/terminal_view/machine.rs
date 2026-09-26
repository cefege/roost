//! The mutable view of the registry the command machine works through.
//!
//! Split out of `registry.rs` for the size cap. It borrows every index at once,
//! which is what lets the command paths in `commands.rs` mutate the view, the
//! session index, the socket index and the claim store without re-deriving a
//! borrow for each.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use roost_protocol::wire::SessionId;

use super::record::{TombstoneStore, ViewRecord};
use super::registry::SocketRecord;
use super::sink::SinkCall;

/// The session a record names, or `None` when it is not a session id.
///
/// Every admitted record's session id passed `is_terminal_uuid` at the trust
/// boundary, so a failure here is a defect; the record is left unindexed rather
/// than filed under a placeholder that would answer a geometry nobody asked for.
pub(super) fn session_of(record: &ViewRecord) -> Option<SessionId> {
    SessionId::try_from(record.intent.session_id.clone()).ok()
}

/// The mutable view of the registry the command machine works through.
pub(super) struct Machine<'a> {
    pub(super) sockets: &'a mut HashMap<String, SocketRecord>,
    pub(super) views: &'a mut HashMap<String, ViewRecord>,
    pub(super) session_views: &'a mut BTreeMap<SessionId, BTreeSet<String>>,
    pub(super) tombstones: &'a mut TombstoneStore,
}

impl Machine<'_> {
    /// How many view records one socket currently owns.
    pub(super) fn view_count(&self, socket_id: &str) -> usize {
        self.sockets
            .get(socket_id)
            .map_or(0, |socket| socket.views.len())
    }

    /// How many view records one session currently holds.
    pub(super) fn session_count(&self, session_id: &SessionId) -> usize {
        self.session_views.get(session_id).map_or(0, BTreeSet::len)
    }

    /// Remove one record and every index that names it. `save` retains a
    /// tombstone so the same tab can reclaim the handle.
    ///
    /// A missing record is not an error: the sweep walks a snapshot and a host
    /// callback can remove a record re-entrantly, and a second removal must not
    /// resurrect a tombstone the first one already wrote.
    pub(super) fn drop_record(&mut self, key: &str, save: bool, now_ms: u64) {
        let Some(record) = self.views.remove(key) else {
            return;
        };
        if let Some(session_id) = session_of(&record)
            && let Some(keys) = self.session_views.get_mut(&session_id)
        {
            keys.remove(key);
            if keys.is_empty() {
                self.session_views.remove(&session_id);
            }
        }
        if let Some(socket) = self.sockets.get_mut(&record.socket_id) {
            socket.views.remove(key);
        }
        if save {
            self.tombstones.retain(
                now_ms,
                key.to_owned(),
                record.viewer_key.clone(),
                record.revision,
                record.intent.clone(),
            );
        }
    }


    /// Report whether the socket still holds any view of this session, so the
    /// host can start or stop feeding it that session's cells.
    ///
    /// The answer is a whole-session question, not a per-record one: two views
    /// of one session on one socket are one feed.
    pub(super) fn sync_watching(
        &mut self,
        socket_id: &str,
        session_id: &SessionId,
    ) -> Option<SinkCall> {
        let watching = self
            .views
            .values()
            .any(|record| record.socket_id == socket_id && session_of(record).as_ref() == Some(session_id));
        Some(SinkCall::Watching {
            socket_id: socket_id.to_owned(),
            session_id: session_id.clone(),
            watching,
        })
    }
}
