//! Admission order for one session's observed agent status: the ONE place that
//! decides whether a report is newer than what is already retained.
//!
//! Lives here, not in either end, because BOTH ends need it and neither end can
//! see the other. The coordinator refuses a late worker frame in
//! `roost-coord/src/agents/status_hub.rs`; a browser refuses a late socket frame
//! in `roost-client-core/src/client/agents/status_projection.rs`. v2 kept two
//! copies of this predicate — `apps/coord/src/agents/agent-status-order.ts` and
//! the hand-rolled `acceptsStatus` in `apps/web/src/store/agent-status.ts` — and
//! a socket that reorders, retries and duplicates is exactly the input that
//! separates them. A rule the client restates is a rule that will drift.
//!
//! WHY REVISIONS ARE NOT A TOTAL ORDER ACROSS OCCUPANTS. A revision counts
//! within one `status_epoch`/`occupant_id` pair. Two different occupants of the
//! same session both number their updates from 1, so comparing revisions across
//! them would let a brand new agent be refused as "stale" by the previous
//! agent's high water mark. Retired identities are compared by EQUALITY only,
//! never by value, and stay fenced through a close/open boundary -- otherwise a
//! reconnecting worker could resurrect the occupant the session just retired.
//!
//! A LEGACY FRAME (one with no identity triple, from a worker deployed before
//! durable observation) yields permanently once any identified occupant has
//! been accepted. Two answers to "what is the state of this session" cannot
//! both be right, and the identified one is the one that can be fenced.

use std::collections::{HashMap, HashSet, VecDeque};

use super::{AgentStatus, AgentStatusFields, AgentStatusIdentity, AgentStatusUpdate, agent_status_identity};
use crate::wire::{AgentOccupantId, AgentRuntimeState, StatusEpoch};

/// Fencing an epoch this many generations old cannot matter -- its occupants are
/// long replaced -- while an unbounded retired set grows one session's order
/// object with every agent restart for the life of the process.
pub const MAX_RETIRED_EPOCHS: usize = 64;

/// Whether two OPTIONAL identities name the exact same occupant.
///
/// The identity-level half of [`same_agent_status_occupant`], so a caller that
/// holds identities rather than statuses — a browser's acknowledgement token,
/// say — compares occupants with the same rule instead of restating it.
#[must_use]
pub fn same_agent_identity_occupant(
    left: Option<&AgentStatusIdentity>,
    right: Option<&AgentStatusIdentity>,
) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => {
            left.status_epoch == right.status_epoch && left.occupant_id == right.occupant_id
        }
        (None, None) => true,
        _ => false,
    }
}

/// Whether two statuses belong to the exact same occupant.
///
/// A status with no identity triple never matches an identified one, in either
/// direction: that asymmetry is what makes a legacy frame unable to displace an
/// identified occupant even at a lower revision.
#[must_use]
pub fn same_agent_status_occupant(left: &AgentStatusFields, right: &AgentStatusFields) -> bool {
    same_agent_identity_occupant(
        agent_status_identity(left).as_ref(),
        agent_status_identity(right).as_ref(),
    )
}

/// One session's admission order, mutated only by whoever owns the retained row.
#[derive(Debug, Default)]
pub struct AgentStatusOrder {
    /// Whether an identified occupant has ever been accepted for this session.
    identified_accepted: bool,
    /// The highest legacy revision seen, so a legacy retry cannot rewind it.
    ///
    /// `None` is "no legacy report has ever been accepted", which is NOT the
    /// same answer as revision 0: a legacy deployment's FIRST report is
    /// revision 0, and a sentinel that reads 0 as a floor refuses it. The
    /// coordinator's copy used a bare `i64` defaulting to 0, so a legacy first
    /// report at revision 0 was dropped there for as long as the sentinel stood.
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
    /// An order seeded from a row this browser was handed rather than observed.
    ///
    /// A hydration seed is the one case where the retained row exists before any
    /// order does, and without this the first frame to arrive would be judged
    /// against nothing: a legacy row at revision 7 re-sent verbatim would be
    /// accepted as progress, and an identified row would be accepted by a
    /// *different* occupant of the same epoch with no retirement recorded.
    #[must_use]
    pub fn seeded_from(previous: Option<&AgentStatus>) -> Self {
        let mut order = Self::default();
        let Some(held) = previous else {
            return order;
        };
        match agent_status_identity(&held.common) {
            Some(identity) => {
                order.identified_accepted = true;
                order.latest_status_epoch = Some(identity.status_epoch);
                order.latest_occupant_id = Some(identity.occupant_id);
                order.latest_state = Some(held.common.state);
                order.latest_state_change_revision = held.common.revision;
            }
            None => order.legacy_revision = Some(held.common.revision),
        }
        order
    }

    /// Whether `update` may be applied over what is currently retained.
    ///
    /// A pure predicate on purpose: the owner calls it before it mutates, so a
    /// refused frame leaves no trace at all -- not in the retained table, not in
    /// the order, not on the wire.
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
                .is_some_and(|floor| update.common.revision <= floor)
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
                        .map_or(inactive_revision, |floor| floor.max(inactive_revision)),
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
