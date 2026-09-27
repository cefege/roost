//! The browser's volatile agent-status projection: retained rows, the fence
//! that decides which report may replace one, and the attention order.
//!
//! THE FENCE IS `roost_protocol::wire::agent_status::order`, not this file.
//! The coordinator refuses a late worker frame with it; a browser refuses a late
//! socket frame with the SAME type. The socket reorders, retries and duplicates,
//! and the coordinator's fence cannot save the client: a frame the coordinator
//! admitted in order can still arrive at a browser out of order after a
//! reconnect, a retry or a second socket. Last-arrival-wins shows an agent as
//! stopped for a second after it finished, and that is the whole product.
//!
//! Three things live here that the shared order does not: the retained rows,
//! the BROWSER-ASSIGNED arrival number, and the closed-session fence.
//!
//! Ported from `apps/web/src/store/agent-status.ts`. Depends on
//! `roost_protocol::wire` and `seen`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use roost_protocol::wire::agent_status::order::AgentStatusOrder;
use roost_protocol::wire::{AgentStatus, AgentStatusUpdate, SessionId};

use crate::client::agents::seen::AgentSeenLedger;
use crate::client::agents::status_policy::derive_agent_status_level;

/// How many closed sessions keep their fence.
///
/// A closed session is refused every report until an authoritative session
/// upsert reopens it, so this is a browser-lifecycle fence, not a cache. v2
/// kept it in an unbounded `Set` for the life of the profile; a tab left open
/// across a few hundred sessions would carry every one of them forever. The
/// oldest goes first, because a session that closed longest ago is the one a
/// late frame is least likely to still be claiming.
pub const CLOSED_SESSION_FENCE_MAX: usize = 256;

/// What one admitted report did to one session's row.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentStatusChange {
    /// The session whose row changed.
    pub session_id: SessionId,
    /// The row as it stood BEFORE this report, or `None`.
    ///
    /// A SNAPSHOT, never a reference into the retained table and never a live
    /// view of it: `docs/FAILURE-INDEX.md:50` — "a store proxy handed to a
    /// subscriber reads the POST-write value" — is a host that mirrors this
    /// table into a reactive store and is handed a node it then mutates. A
    /// `previous` that is not a detached copy reports every transition as a
    /// self-transition, and the notification that never fires is the symptom.
    pub previous: Option<AgentStatus>,
    /// The row as it stands after this report, or `None` when the row is gone.
    pub next: Option<AgentStatus>,
    /// The revision this report carried. The order, not the clock: two
    /// machines' reports cannot be compared by arrival.
    pub revision: i64,
}

/// One browser-assigned arrival number.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Arrival {
    /// The identity this number was assigned to, so a re-sent identical report
    /// does not take a new place in the attention order.
    identity: String,
    arrival: u64,
}

/// Every agent status this client knows, and the fences that order them.
#[derive(Debug, Default)]
pub struct AgentStatusProjection {
    /// The retained rows, in session-id order. `BTreeMap` because THE ORDER IS
    /// PART OF THE ANSWER: a client that folds broadcasts and a client that
    /// re-hydrates must converge, and one sorted table is the cheapest way to
    /// guarantee it.
    statuses: BTreeMap<SessionId, AgentStatus>,
    /// Per-session admission order, kept on past the row it fences — the same
    /// reason the coordinator keeps its own: a dropped row's revision floor is
    /// what makes the report that brings it back a stale one.
    order: BTreeMap<SessionId, AgentStatusOrder>,
    /// Sessions the durable projector removed. Every report for one is refused
    /// until an authoritative upsert reopens it, so a frame in flight when a
    /// session closed cannot resurrect a row for a session that is gone.
    closed: BTreeSet<SessionId>,
    /// Closed sessions in insertion order, so the oldest can be dropped when
    /// the fence is full. One structure, scanned: a membership index beside it
    /// would be a second answer to "is this session closed".
    closed_order: VecDeque<SessionId>,
    /// Where each session's current status sits in attention order.
    arrivals: BTreeMap<SessionId, Arrival>,
    /// The last number handed out. Monotonic per browser, so a worker whose wall
    /// clock runs ahead cannot pin its sessions to the top of an attention list.
    last_arrival: u64,
}

impl AgentStatusProjection {
    /// A projection that has never seen an agent.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A projection seeded from rows a host hydrated, rather than observed.
    ///
    /// The seed's own revisions are already in the retained rows, so each
    /// session's order starts fenced to its row: without this the first frame
    /// to arrive is judged against an empty order and a re-sent seed would be
    /// read as progress.
    #[must_use]
    pub fn seeded(statuses: BTreeMap<SessionId, AgentStatus>) -> Self {
        let order = statuses
            .iter()
            .map(|(session_id, status)| {
                (
                    session_id.clone(),
                    AgentStatusOrder::seeded_from(Some(status)),
                )
            })
            .collect();
        Self {
            statuses,
            order,
            ..Self::default()
        }
    }

    /// The retained rows, for a host that renders them.
    #[must_use]
    pub fn statuses(&self) -> &BTreeMap<SessionId, AgentStatus> {
        &self.statuses
    }

    /// One session's retained row.
    #[must_use]
    pub fn status(&self, session_id: &SessionId) -> Option<&AgentStatus> {
        self.statuses.get(session_id)
    }

    /// How many sessions carry a status.
    #[must_use]
    pub fn len(&self) -> usize {
        self.statuses.len()
    }

    /// Whether no session carries a status.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.statuses.is_empty()
    }

    /// This browser's arrival order for a session's current status; `0` when it
    /// has none.
    ///
    /// The ordering key for attention across machines. Not the report's own
    /// revision, and not its timestamp: revisions count within one occupant and
    /// timestamps are the producing worker's clock, so neither can order a list
    /// that spans machines.
    #[must_use]
    pub fn arrival(&self, session_id: &SessionId) -> u64 {
        self.arrivals
            .get(session_id)
            .map_or(0, |arrival| arrival.arrival)
    }

    /// Whether a session is fenced closed.
    #[must_use]
    pub fn is_closed(&self, session_id: &SessionId) -> bool {
        self.closed.contains(session_id)
    }

    /// Validate, fence, and project one agent-status report.
    ///
    /// Returns the change it made, or `None` when it made none: a refused
    /// report leaves no trace at all — not in the table, not in the order, not
    /// in the attention order. `seen` supplies what THIS profile has already
    /// acknowledged, which is the only thing that can retire a released
    /// occupant's row.
    pub fn apply_update(
        &mut self,
        update: &AgentStatusUpdate,
        seen: &AgentSeenLedger,
    ) -> Option<AgentStatusChange> {
        if update.common.check().is_err() {
            return None;
        }
        let session_id = update.common.session_id.clone();
        if self.closed.contains(&session_id) {
            return None;
        }
        let current = self.statuses.get(&session_id).cloned();
        let order = self
            .order
            .entry(session_id.clone())
            .or_insert_with(|| AgentStatusOrder::seeded_from(current.as_ref()));
        if !order.accepts(current.as_ref(), update) {
            return None;
        }
        order.record(update);

        let previous = current.clone();
        let retained = if update.active {
            // An ACTIVE status is the one shape that is always retained: the
            // update is validated above, so this is the same construction the
            // wire type validates, not a second parse of it.
            let status = AgentStatus {
                common: update.common.clone(),
                active: true,
            };
            (!released_occupant_is_spent(&status, seen)).then_some(status)
        } else {
            None
        };
        match &retained {
            Some(status) => {
                self.record_arrival(update);
                self.statuses.insert(session_id.clone(), status.clone());
            }
            None => {
                self.arrivals.remove(&session_id);
                self.statuses.remove(&session_id);
            }
        }
        Some(AgentStatusChange {
            session_id,
            previous,
            next: retained,
            revision: update.common.revision,
        })
    }

    /// Remove a session's volatile status because the durable projector removed
    /// its row, and fence the session closed.
    ///
    /// This is the lifecycle fence. It returns `None` when there was no status
    /// to remove, but the fence is installed either way: a session the durable
    /// plane has dropped must refuse a report even if this client never held a
    /// row for it.
    pub fn clear_session(&mut self, session_id: &SessionId) -> Option<AgentStatusChange> {
        self.fence_closed(session_id);
        let current = self.statuses.remove(session_id)?;
        if let Some(order) = self.order.get_mut(session_id) {
            order.record_close(&current, current.common.revision);
        }
        self.arrivals.remove(session_id);
        Some(AgentStatusChange {
            session_id: session_id.clone(),
            previous: Some(current.clone()),
            next: None,
            revision: current.common.revision,
        })
    }

    /// An authoritative session upsert starts a fresh browser lifecycle fence.
    pub fn mark_session_open(&mut self, session_id: &SessionId) {
        self.closed.remove(session_id);
        if let Some(closed) = self.closed_order.iter().position(|closed| closed == session_id) {
            self.closed_order.remove(closed);
        }
        // Clearing `closed` is NOT enough to reopen a session. `clear_session`
        // also retires the occupant through `AgentStatusOrder::record_close`,
        // and that retirement outlives the closed set: the next report reuses
        // the existing order, `accepts` finds the occupant retired, and the
        // upsert is refused — so the fence this function documents starting
        // never starts. Dropping the order is the whole reopen. The next update
        // re-seeds it from whatever is retained, so a session that was never
        // closed keeps exactly the ordering it had.
        self.order.remove(session_id);
    }

    /// Retire released occupants this profile has nothing left to show for.
    ///
    /// A released occupant's row is retained only to carry the completion that
    /// occupant earned; presented as anything but `Done` it describes an agent
    /// that is gone and a completion this profile already acknowledged, so it is
    /// spent. The sweep is driven from the ACKNOWLEDGEMENT side, not from the
    /// frame that produced the row, because acknowledgement also arrives from
    /// another tab.
    pub fn retire_spent_released(&mut self, seen: &AgentSeenLedger) -> Vec<AgentStatusChange> {
        let spent: Vec<SessionId> = self
            .statuses
            .iter()
            .filter(|(_, status)| released_occupant_is_spent(status, seen))
            .map(|(session_id, _)| session_id.clone())
            .collect();
        spent
            .into_iter()
            .filter_map(|session_id| {
                let current = self.statuses.remove(&session_id)?;
                self.arrivals.remove(&session_id);
                Some(AgentStatusChange {
                    session_id,
                    previous: Some(current.clone()),
                    next: None,
                    revision: current.common.revision,
                })
            })
            .collect()
    }

    /// Drop every identity fence and every row, reporting what each row was.
    ///
    /// A host calls this on a credential boundary: a delayed notification timer
    /// must not be able to outlive the credential that scheduled it, and a
    /// retained row is what such a timer reads.
    pub fn reset(&mut self) -> Vec<AgentStatusChange> {
        let previous = std::mem::take(&mut self.statuses);
        self.order.clear();
        self.arrivals.clear();
        self.last_arrival = 0;
        self.closed.clear();
        self.closed_order.clear();
        previous
            .into_iter()
            .map(|(session_id, status)| AgentStatusChange {
                session_id,
                previous: Some(status.clone()),
                next: None,
                revision: status.common.revision,
            })
            .collect()
    }

    /// The retained rows that a released-occupant sweep would retire, for a
    /// host that wants to know before it acts.
    #[must_use]
    pub fn spent_released_count(&self, seen: &AgentSeenLedger) -> usize {
        self.statuses
            .values()
            .filter(|status| released_occupant_is_spent(status, seen))
            .count()
    }

    fn fence_closed(&mut self, session_id: &SessionId) {
        if !self.closed.insert(session_id.clone()) {
            return;
        }
        self.closed_order.push_back(session_id.clone());
        while self.closed_order.len() > CLOSED_SESSION_FENCE_MAX {
            if let Some(oldest) = self.closed_order.pop_front() {
                self.closed.remove(&oldest);
            }
        }
    }

    fn record_arrival(&mut self, update: &AgentStatusUpdate) {
        let identity = arrival_identity(update);
        let session_id = update.common.session_id.clone();
        if self
            .arrivals
            .get(&session_id)
            .is_some_and(|arrival| arrival.identity == identity)
        {
            return;
        }
        self.last_arrival += 1;
        self.arrivals.insert(
            session_id,
            Arrival {
                identity,
                arrival: self.last_arrival,
            },
        );
    }
}

/// The identity an arrival number was assigned to.
///
/// The revision is part of it because two reports from one occupant are two
/// different moments in the attention order, while a socket that re-sends the
/// same report verbatim is one moment observed twice.
fn arrival_identity(update: &AgentStatusUpdate) -> String {
    match roost_protocol::wire::agent_status::agent_status_identity(&update.common) {
        Some(identity) => format!(
            "{}:{}:{}",
            identity.status_epoch.as_str(),
            identity.occupant_id.as_str(),
            update.common.revision
        ),
        None => format!(":{}", update.common.revision),
    }
}

/// A released occupant's row is spent once this profile has acknowledged the
/// completion it earned.
fn released_occupant_is_spent(status: &AgentStatus, seen: &AgentSeenLedger) -> bool {
    status.common.occupant_exited
        && derive_agent_status_level(
            Some(status),
            Some(seen.acknowledged_revision(status)),
        ) != crate::client::agents::status_policy::AgentStatusLevel::Done
}
