//! Terminal view membership: which socket watches which session's terminal,
//! lease renewal, parking on socket loss, and the sweep that reaps both.
//!
//! Ported from `packages/protocol/src/terminal-view/terminal-view-registry.ts`
//! and its operations sibling. The command state machine is in `commands.rs`;
//! this file owns the maps, the socket lifecycle and the lease sweep.
//!
//! WHY THE MACHINE NEVER CALLS OUT. Every effect is returned as a
//! [`SinkCall`] or a [`PendingReply`] for the hub to perform after it releases
//! this registry, because the recompute hook reads the registry to recompute and
//! a self-call under the same lock would deadlock.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt;
use std::sync::Arc;

use roost_proto::TerminalViewStatus;
use roost_protocol::viewport::TerminalGeometry;
use roost_protocol::wire::SessionId;

use super::machine::{Machine, session_of};
use super::record::{
    GeometrySet, TombstoneStore, ViewInput, ViewRecord, ViewStats, active_fingerprints,
    geometry_set, project_inputs, project_viewers, view_constrains,
};
use super::sink::{PendingReply, SinkCall, TerminalViewSink};

/// The authenticated facts one Sync socket contributes to view membership.
#[derive(Clone)]
pub struct SocketRegistration {
    /// The socket the Sync session minted.
    pub socket_id: String,
    /// The `${fingerprint}:${tab}` key that owns socket-bound view handles.
    /// `None` for a socket that may not hold views at all.
    pub viewer_key: Option<String>,
    /// The verified device fingerprint.
    pub caller_fingerprint: String,
    /// The sessions this socket may observe, as resolved at upgrade.
    pub session_ids: BTreeSet<String>,
    /// Where the socket's view-state frames go.
    pub sink: Arc<dyn TerminalViewSink>,
}

/// One registered socket, with the view keys it currently owns.
#[derive(Clone)]
pub struct SocketRecord {
    /// The socket id.
    pub id: String,
    /// The tab-bound viewer key, when the socket has one.
    pub viewer_key: Option<String>,
    /// The verified device fingerprint.
    pub fingerprint: String,
    /// The sessions this socket may observe.
    pub allowed_sessions: BTreeSet<String>,
    /// Where its frames go.
    pub sink: Arc<dyn TerminalViewSink>,
    /// The view keys this socket currently owns.
    pub views: BTreeSet<String>,
}

impl SocketRecord {
    /// Whether this socket was admitted to observe `session_id`.
    #[must_use]
    pub fn allows(&self, session_id: &str) -> bool {
        self.allowed_sessions.contains(session_id)
    }
}

/// What one membership mutation decided: the answers its sockets are owed, the
/// host effects to perform, and the sessions whose effective geometry changed.
#[derive(Debug, Default)]
pub struct MembershipOutcome {
    /// Sessions whose live viewer set moved, so their effective geometry is
    /// recomputed before any record reply is built.
    pub changed: BTreeSet<SessionId>,
    /// The answers owed, in decision order.
    pub replies: Vec<PendingReply>,
    /// The host effects owed, in decision order.
    pub calls: Vec<SinkCall>,
}

impl MembershipOutcome {
    /// Whether anything at all changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.replies.is_empty() && self.calls.is_empty()
    }

    /// Answer a view record once the hub knows the session's effective geometry.
    pub(super) fn answer_view(
        &mut self,
        record: &ViewRecord,
        status: TerminalViewStatus,
        reason: &str,
    ) {
        self.replies.push(PendingReply::View {
            socket_id: record.socket_id.clone(),
            view_id: record.view_id.clone(),
            session_id: record.intent.session_id.clone(),
            revision: record.revision,
            status,
            reason: reason.to_owned(),
        });
    }
}

/// The coordinator's terminal view membership.
#[derive(Default)]
pub struct ViewRegistry {
    sockets: HashMap<String, SocketRecord>,
    views: HashMap<String, ViewRecord>,
    session_views: BTreeMap<SessionId, BTreeSet<String>>,
    tombstones: TombstoneStore,
}

impl fmt::Debug for ViewRegistry {
    /// A log line needs the counts it would change, not the records.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ViewRegistry")
            .field("sockets", &self.sockets.len())
            .field("views", &self.views.len())
            .field("sessions", &self.session_views.len())
            .field("tombstones", &self.tombstones.len())
            .finish()
    }
}

impl ViewRegistry {
    /// A registry with no sockets and no records.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Admit a socket, replacing any earlier registration of the same id.
    pub fn register_socket(&mut self, registration: &SocketRegistration, now_ms: u64) {
        self.close_socket(&registration.socket_id, now_ms);
        self.sockets.insert(
            registration.socket_id.clone(),
            SocketRecord {
                id: registration.socket_id.clone(),
                viewer_key: registration.viewer_key.clone(),
                fingerprint: registration.caller_fingerprint.clone(),
                allowed_sessions: registration.session_ids.clone(),
                sink: Arc::clone(&registration.sink),
                views: BTreeSet::new(),
            },
        );
    }

    /// A socket is gone: park every record it owned and keep them in membership
    /// until their lease reaps them.
    ///
    /// No recompute happens here on purpose. The park grace defers the geometry
    /// drop to the sweep tick that observes it, so one owner decides when a
    /// parked viewer stops binding the PTY.
    pub fn close_socket(&mut self, socket_id: &str, now_ms: u64) {
        let Some(socket) = self.sockets.remove(socket_id) else {
            return;
        };
        for key in &socket.views {
            if let Some(record) = self.views.get_mut(key)
                && record.socket_id == socket_id
            {
                record.parked = true;
                record.parked_at_ms = now_ms;
            }
        }
    }

    /// A device lost its authority: every record, claim and socket it held goes.
    pub fn remove_fingerprint(&mut self, fingerprint: &str) -> MembershipOutcome {
        let mut outcome = MembershipOutcome::default();
        let doomed: Vec<(String, Option<SessionId>)> = self
            .views
            .iter()
            .filter(|(_, record)| record.fingerprint == fingerprint)
            .map(|(key, record)| (key.clone(), session_of(record)))
            .collect();
        for (key, session_id) in &doomed {
            self.machine().drop_record(key, false, 0);
            if let Some(session_id) = session_id.clone() {
                outcome.changed.insert(session_id);
            }
        }
        self.tombstones.remove_viewer(fingerprint);
        self.sockets
            .retain(|_, socket| socket.fingerprint != fingerprint);
        tracing::info!(
            fingerprint,
            records = doomed.len(),
            "terminal views revoked for a device"
        );
        outcome
    }

    /// The sockets one device currently holds, for a revocation that has to
    /// stop reaching owners before it touches any record.
    #[must_use]
    pub fn socket_ids_for_fingerprint(&self, fingerprint: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .sockets
            .iter()
            .filter(|(_, socket)| socket.fingerprint == fingerprint)
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }


    /// A session closed: no record and no retained claim of it survives.
    pub fn close_session(&mut self, session_id: &SessionId) -> MembershipOutcome {
        let mut outcome = MembershipOutcome::default();
        if let Some(keys) = self.session_views.get(session_id).cloned() {
            for key in keys {
                self.machine().drop_record(&key, false, 0);
            }
        }
        self.session_views.remove(session_id);
        self.tombstones.remove_session(session_id.as_str());
        tracing::info!(%session_id, "terminal view membership released for a closed session");
        outcome
    }

    /// One sweep tick: reap lapsed leases, drop every record whose park grace
    /// has just elapsed, and expire tombstones.
    pub fn sweep(&mut self, now_ms: u64) -> MembershipOutcome {
        let mut outcome = MembershipOutcome::default();
        let keys: Vec<String> = self.views.keys().cloned().collect();
        for key in keys {
            let Some(record) = self.views.get(&key).cloned() else {
                continue;
            };
            if record.deadline_ms > now_ms {
                if record.constrains && !view_constrains(&record, now_ms) {
                    tracing::info!(
                        session_id = %record.intent.session_id,
                        view_id = %record.view_id,
                        cols = record.intent.cols,
                        rows = record.intent.rows,
                        "terminal view park grace lapsed"
                    );
                    if let Some(record) = self.views.get_mut(&key) {
                        record.constrains = false;
                    }
                    if let Some(session_id) = session_of(&record) {
                        outcome.changed.insert(session_id);
                    }
                }
                continue;
            }
            if !record.parked {
                if let Some(session_id) = session_of(&record) {
                    outcome.calls.push(SinkCall::LiveViewExpired {
                        socket_id: record.socket_id.clone(),
                        view_id: record.view_id.clone(),
                        session_id,
                    });
                }
            }
            self.machine().drop_record(&key, true, now_ms);
            if let Some(session_id) = session_of(&record) {
                outcome.changed.insert(session_id);
            }
        }
        self.tombstones.expire(now_ms);
        outcome
    }

    /// The records that bind one session's PTY right now, and how many records
    /// membership still holds for it.
    #[must_use]
    pub fn geometry_set(&self, session_id: &SessionId, now_ms: u64) -> GeometrySet {
        geometry_set(&self.views, self.session_views.get(session_id), now_ms)
    }

    /// The per-viewer diagnostic rows for one session.
    #[must_use]
    pub fn viewer_inputs(&self, session_id: &SessionId, now_ms: u64) -> Vec<ViewInput> {
        project_inputs(&self.views, self.session_views.get(session_id), now_ms)
    }

    /// How many of one session's records are live and how many are parked.
    #[must_use]
    pub fn view_stats(&self, session_id: &SessionId) -> ViewStats {
        let mut stats = ViewStats::default();
        for record in self
            .session_views
            .get(session_id)
            .into_iter()
            .flatten()
            .filter_map(|key| self.views.get(key))
        {
            if record.parked {
                stats.parked += 1;
            } else {
                stats.active += 1;
            }
        }
        stats
    }

    /// The devices holding a record on this session.
    #[must_use]
    pub fn active_fingerprints(&self, session_id: &SessionId) -> BTreeSet<String> {
        active_fingerprints(&self.views, self.session_views.get(session_id))
    }

    /// Every session's per-device viewer geometry.
    #[must_use]
    pub fn viewer_projection(&self) -> BTreeMap<SessionId, BTreeMap<String, TerminalGeometry>> {
        project_viewers(&self.session_views, &self.views)
    }

    /// One registered socket, if it is still here.
    #[must_use]
    pub fn socket(&self, socket_id: &str) -> Option<&SocketRecord> {
        self.sockets.get(socket_id)
    }

    /// The mutable view the command machine works through.
    pub(super) fn machine(&mut self) -> Machine<'_> {
        Machine {
            sockets: &mut self.sockets,
            views: &mut self.views,
            session_views: &mut self.session_views,
            tombstones: &mut self.tombstones,
        }
    }
}
