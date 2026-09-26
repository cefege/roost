//! Admission order for one session's observed agent status: the single place
//! that decides whether a report from a worker is newer than what is retained.
//!
//! Ported from `apps/coord/src/agents/agent-status-order.ts`. The hub calls
//! [`AgentStatusOrder::accepts`] before it mutates anything, so a late,
//! reordered or replayed frame is dropped rather than applied.
//!
//! WHY REVISIONS ARE NOT A TOTAL ORDER ACROSS OCCUPANTS. A revision counts
//! within one `status_epoch`/`occupant_id` pair. Two different occupants of the
//! same session both number their updates from 1, so comparing revisions across
//! them would let a brand new agent be refused as "stale" by the previous
//! agent's high water mark. Retired identities are compared by EQUALITY only,
//! never by value, and stay fenced through a close/open boundary -- otherwise
//! a reconnecting worker could resurrect the occupant the session just retired.
//!
//! A LEGACY FRAME (one with no identity triple, from a worker deployed before
//! durable observation) yields permanently once any identified occupant has
//! been accepted. Two answers to "what is the state of this session" cannot
//! both be right, and the identified one is the one that can be fenced.

use std::collections::{HashMap, HashSet, VecDeque};

use roost_protocol::wire::{
    AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusFields, AgentStatusIdentity,
    AgentStatusUpdate, StatusEpoch,
};
use roost_protocol::wire::agent_status::agent_status_identity;

/// Fencing an epoch this many generations old cannot matter -- its occupants are
/// long replaced -- while an unbounded retired set grows one session's order
/// object with every agent restart for the life of the process.
pub const MAX_RETIRED_EPOCHS: usize = 64;

/// Whether two statuses belong to the exact same occupant.
///
/// A status with no identity triple never matches an identified one, in either
/// direction: that asymmetry is what makes a legacy frame unable to displace an
/// identified occupant even at a lower revision.
#[must_use]
pub fn same_agent_status_occupant(left: &AgentStatusFields, right: &AgentStatusFields) -> bool {
    let left_identified = agent_status_identity(left);
    let right_identified = agent_status_identity(right);
    match (left_identified, right_identified) {
        (Some(left), Some(right)) => {
            left.status_epoch == right.status_epoch && left.occupant_id == right.occupant_id
        }
        (None, None) => true,
        _ => false,
    }
}

/// One session's admission order, mutated only by the status hub.
#[derive(Debug, Default)]
pub struct AgentStatusOrder {
    /// Whether an identified occupant has ever been accepted for this session.
    identified_accepted: bool,
    /// The highest legacy revision seen, so a legacy retry cannot rewind it.
    ///
    /// `None` is "no legacy report has ever been accepted", which is NOT the
    /// same as `Some(0)`. `AgentStatusFields::check` admits revision 0, and a
    /// legacy deployment's FIRST report is revision 0 -- so a plain `i64`
    /// defaulting to 0 refuses exactly the report that opens the sequence, and
    /// a legacy session never becomes visible at all.
    legacy_revision: Option<i64>,
    /// Retired epochs, oldest first. A `Vec` rather than a set because the
    /// eviction rule is positional (FIFO past [`MAX_RETIRED_EPOCHS`]) and a
    /// second membership structure would be a second answer to "is this
    /// epoch retired".
    retired_epochs: VecDeque<StatusEpoch>,
    /// Retired occupants within a still-current epoch.
    retired_occupants_by_epoch: HashMap<StatusEpoch, HashSet<AgentOccupantId>>,
    /// The identity the last identified update carried.
    latest_status_epoch: Option<StatusEpoch>,
    latest_occupant_id: Option<AgentOccupantId>,
    /// The state last observed for that identity, which is what makes a
    /// revision bump distinguishable from a real transition.
    latest_state: Option<AgentRuntimeState>,
    /// The revision at which that state last actually changed. The only
    /// advance a status wait over an active state may honour.
    latest_state_change_revision: i64,
}

impl AgentStatusOrder {
    /// Whether `update` may be applied over what is currently retained.
    ///
    /// A pure predicate on purpose: the hub calls it before it mutates, so a
    /// refused frame leaves no trace at all -- not in the retained table, not
    /// in the order, not on the wire.
    #[must_use]
    pub fn accepts(&self, previous: Option<&AgentStatus>, update: &AgentStatusUpdate) -> bool {
        let Some(identity) = agent_status_identity(&update.common) else {
            if self.identified_accepted
                || previous.is_some_and(|held| agent_status_identity(&held.common).is_some())
            {
                return false;
            }
            if self
                .legacy_revision
                .is_some_and(|highest| update.common.revision <= highest)
            {
                return false;
            }
            // A legacy deletion with nothing retained is a no-op, not a
            // transition: accepting it would publish a tombstone for a
            // session that never had a status.
            return update.active || previous.is_some();
        };
        if self.is_retired(&identity) {
            return false;
        }
        let Some(held) = previous else {
            return update.active;
        };
        if agent_status_identity(&held.common).is_none() {
            return update.active;
        }
        if !same_agent_status_occupant(&held.common, &update.common) {
            return update.active;
        }
        update.common.revision > held.common.revision
    }

    /// Fold an ACCEPTED update into the order.
    pub fn record(&mut self, update: &AgentStatusUpdate) {
        let Some(identity) = agent_status_identity(&update.common) else {
            // `accepts` refused anything at or below the floor, so this
            // assignment cannot lower it and needs no `max`. The one
            // distinction is the first write: `None` becomes `Some(0)`, which
            // is a revision that was accepted rather than a sentinel.
            self.legacy_revision = Some(update.common.revision);
            return;
        };
        // Waiters advance on a real transition, so the change point moves only
        // when this occupant's state differs from the state last observed for
        // it. A worker republishes an occupant whenever its message or its
        // authority source changes, and that is not progress.
        if self.latest_status_epoch.as_ref() != Some(&identity.status_epoch)
            || self.latest_occupant_id.as_ref() != Some(&identity.occupant_id)
            || self.latest_state != Some(update.common.state)
        {
            self.latest_state_change_revision = update.common.revision;
        }
        self.latest_state = Some(update.common.state);
        self.identified_accepted = true;
        self.advance_identity(&identity);
        if !update.active {
            self.retire_occupant(&identity.status_epoch, &identity.occupant_id);
        }
    }

    /// The revision at which the latest occupant's state last actually changed.
    #[must_use]
    pub fn state_change_revision(&self) -> i64 {
        self.latest_state_change_revision
    }

    /// Fold the synthetic deletion a session close publishes.
    ///
    /// The identity is retired here rather than by an update, because a close
    /// publishes no worker frame: without this the occupant the session just
    /// lost would be acceptable again the moment the session reopened.
    pub fn record_close(&mut self, current: &AgentStatus, inactive_revision: i64) {
        match agent_status_identity(&current.common) {
            Some(identity) => {
                self.identified_accepted = true;
                self.advance_identity(&identity);
                self.retire_occupant(&identity.status_epoch, &identity.occupant_id);
            }
            None => {
                self.legacy_revision = Some(
                    self.legacy_revision
                        .unwrap_or(i64::MIN)
                        .max(inactive_revision),
                );
            }
        }
    }

    fn is_retired(&self, identity: &AgentStatusIdentity) -> bool {
        self.retired_epochs.contains(&identity.status_epoch)
            || self
                .retired_occupants_by_epoch
                .get(&identity.status_epoch)
                .is_some_and(|occupants| occupants.contains(&identity.occupant_id))
    }

    fn advance_identity(&mut self, identity: &AgentStatusIdentity) {
        if let Some(previous_epoch) = self.latest_status_epoch.clone() {
            if previous_epoch != identity.status_epoch {
                self.retire_epoch(&previous_epoch);
            } else if let Some(previous_occupant) = self.latest_occupant_id.clone() {
                if previous_occupant != identity.occupant_id {
                    self.retire_occupant(&previous_epoch, &previous_occupant);
                }
            }
        }
        self.latest_status_epoch = Some(identity.status_epoch.clone());
        self.latest_occupant_id = Some(identity.occupant_id.clone());
    }

    fn retire_occupant(&mut self, status_epoch: &StatusEpoch, occupant_id: &AgentOccupantId) {
        self.retired_occupants_by_epoch
            .entry(status_epoch.clone())
            .or_default()
            .insert(occupant_id.clone());
    }

    fn retire_epoch(&mut self, status_epoch: &StatusEpoch) {
        if !self.retired_epochs.contains(status_epoch) {
            self.retired_epochs.push_back(status_epoch.clone());
        }
        self.retired_occupants_by_epoch.remove(status_epoch);
        if self.retired_epochs.len() <= MAX_RETIRED_EPOCHS {
            return;
        }
        // Oldest first, so the fence that has been superseded longest is the
        // one that goes: bounding this set is what keeps one session's order
        // object from growing with every agent restart for process lifetime.
        if let Some(oldest) = self.retired_epochs.pop_front() {
            self.retired_occupants_by_epoch.remove(&oldest);
        }
    }
}
