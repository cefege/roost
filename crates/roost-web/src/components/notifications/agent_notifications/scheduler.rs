//! Which agent status TRANSITION earns a card, and the debounce that rechecks
//! the exact occupant and revision before the card is raised. Pure: the timer
//! is the caller's, so `agent_notifications` arms a real one per ticket and the
//! tests stand in for it. Ports `apps/web/src/lib/agentNotificationCore.ts`;
//! depends on the status identity rules in `roost_protocol::wire::agent_status`.

use std::collections::BTreeMap;

use roost_client_core::client::agents::AgentStatusRevisionToken;
use roost_client_core::client::agents::status_policy::agent_status_revision_token;
use roost_protocol::wire::agent_status::agent_status_identity;
use roost_protocol::wire::agent_status::order::{
    same_agent_identity_occupant, same_agent_status_occupant,
};
use roost_protocol::wire::{AgentRuntimeState, AgentStatus, is_identified_agent_status};

/// How long a transition waits before its card is raised, v2's number: a
/// blocked flicker the agent resolves within it never interrupts anyone.
pub const AGENT_NOTIFICATION_DELAY_MS: u64 = 1_000;

/// What a delivery is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentNotificationKind {
    /// The agent is waiting for a human.
    Blocked,
    /// The agent finished.
    Done,
}

/// One card owed, pinned to the occupant and revision that earned it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentNotificationDelivery {
    /// The session the card is about.
    pub session_id: String,
    /// The attention revision the card spends: the blocked revision, or the
    /// completion revision for a done card.
    pub token: AgentStatusRevisionToken,
    /// The newest status revision that must still be current when the timer
    /// fires; same-occupant metadata revisions move it, nothing else does.
    pub status_revision: i64,
    /// Which card.
    pub kind: AgentNotificationKind,
    /// The completion a done card announces.
    pub completed_revision: Option<i64>,
}

/// A timer the caller owes the scheduler: wake after
/// [`AGENT_NOTIFICATION_DELAY_MS`] and hand both fields to
/// [`AgentNotificationScheduler::take_due`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmedNotification {
    /// The session whose delivery is pending.
    pub session_id: String,
    /// Which arming this timer belongs to; a replaced delivery's timer is spent.
    pub ticket: u64,
}

/// One pending delivery and the arming that owns it.
#[derive(Debug)]
struct PendingNotification {
    delivery: AgentNotificationDelivery,
    ticket: u64,
}

/// The ordered transition scheduler: at most one pending delivery per session.
#[derive(Debug, Default)]
pub struct AgentNotificationScheduler {
    pending: BTreeMap<String, PendingNotification>,
    last_ticket: u64,
}

impl AgentNotificationScheduler {
    /// Read one observed change of a session's status. `previous` is the status
    /// this profile last observed, `next` the one it observes now (`None` once
    /// the row is gone), and `viewed` whether the session is on screen in an
    /// attended tab. Returns the timer to arm when this change starts a new
    /// pending delivery.
    pub fn observe(
        &mut self,
        session_id: &str,
        previous: Option<&AgentStatus>,
        next: Option<&AgentStatus>,
        viewed: bool,
    ) -> Option<ArmedNotification> {
        let Some(next) = next else {
            self.cancel(session_id);
            return None;
        };
        if viewed {
            self.cancel(session_id);
            return None;
        }
        if let Some(pending) = self.pending.get_mut(session_id)
            && can_carry_pending(&pending.delivery, previous, next)
        {
            pending.delivery.status_revision = next.common.revision;
            return None;
        }
        self.cancel(session_id);
        let kind = classify_agent_transition(previous, next)?;
        let completed_revision =
            (kind == AgentNotificationKind::Done).then_some(next.common.completed_revision);
        let mut token = agent_status_revision_token(next);
        if let Some(completed) = completed_revision {
            token.revision = completed;
        }
        self.last_ticket += 1;
        let ticket = self.last_ticket;
        self.pending.insert(
            session_id.to_owned(),
            PendingNotification {
                delivery: AgentNotificationDelivery {
                    session_id: session_id.to_owned(),
                    token,
                    status_revision: next.common.revision,
                    kind,
                    completed_revision,
                },
                ticket,
            },
        );
        Some(ArmedNotification {
            session_id: session_id.to_owned(),
            ticket,
        })
    }

    /// The delivery `armed` was set for, removed from the pending set, or
    /// `None` when a later change cancelled or replaced it.
    pub fn take_due(&mut self, armed: &ArmedNotification) -> Option<AgentNotificationDelivery> {
        if self.pending.get(&armed.session_id)?.ticket != armed.ticket {
            return None;
        }
        self.pending
            .remove(&armed.session_id)
            .map(|pending| pending.delivery)
    }

    /// Drop `session_id`'s pending delivery, returning it.
    pub fn cancel(&mut self, session_id: &str) -> Option<AgentNotificationDelivery> {
        self.pending
            .remove(session_id)
            .map(|pending| pending.delivery)
    }

    /// How many deliveries are waiting on a timer.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }
}

/// Which card, if any, the change from `previous` to `next` earns. Only an
/// ordered transition of ONE occupant counts: a first sighting, a replacement
/// agent, and a status that merely stayed where it was earn nothing, so a
/// blocked row re-read on every store revision raises its card once.
#[must_use]
pub fn classify_agent_transition(
    previous: Option<&AgentStatus>,
    next: &AgentStatus,
) -> Option<AgentNotificationKind> {
    let previous = previous?;
    if previous.common.session_id != next.common.session_id
        || previous.common.agent_id != next.common.agent_id
        || !same_agent_status_occupant(&previous.common, &next.common)
    {
        return None;
    }
    match (previous.common.state, next.common.state) {
        (AgentRuntimeState::Working, AgentRuntimeState::Blocked) => {
            Some(AgentNotificationKind::Blocked)
        }
        (AgentRuntimeState::Working | AgentRuntimeState::Blocked, AgentRuntimeState::Idle)
            if next.common.completed_revision == next.common.revision
                && next.common.completed_revision > previous.common.completed_revision =>
        {
            Some(AgentNotificationKind::Done)
        }
        _ => None,
    }
}

/// Whether `status` is still exactly the revision `delivery` was earned by,
/// so a timer that fires after the agent moved on raises nothing.
#[must_use]
pub fn matches_agent_notification(
    status: Option<&AgentStatus>,
    delivery: &AgentNotificationDelivery,
) -> bool {
    let Some(status) = status else {
        return false;
    };
    if status.common.session_id != delivery.token.session_id
        || status.common.revision != delivery.status_revision
        || !same_agent_identity_occupant(
            agent_status_identity(&status.common).as_ref(),
            delivery.token.identity.as_ref(),
        )
    {
        return false;
    }
    match delivery.kind {
        AgentNotificationKind::Blocked => status.common.state == AgentRuntimeState::Blocked,
        AgentNotificationKind::Done => {
            status.common.state == AgentRuntimeState::Idle
                && delivery.completed_revision == Some(status.common.completed_revision)
        }
    }
}

/// Whether a same-occupant metadata revision (a new message, a source switch)
/// may keep the pending timer instead of restarting it. Any lifecycle change —
/// a new state, a new completion, a new occupant, a legacy row — may not.
fn can_carry_pending(
    delivery: &AgentNotificationDelivery,
    previous: Option<&AgentStatus>,
    next: &AgentStatus,
) -> bool {
    let Some(previous) = previous else {
        return false;
    };
    let carried = delivery.token.identity.is_some()
        && is_identified_agent_status(&previous.common)
        && is_identified_agent_status(&next.common)
        && delivery.status_revision == previous.common.revision
        && previous.common.session_id == next.common.session_id
        && previous.common.agent_id == next.common.agent_id
        && previous.common.state == next.common.state
        && previous.common.completed_revision == next.common.completed_revision
        && previous.active
        && next.active
        && same_agent_identity_occupant(
            delivery.token.identity.as_ref(),
            agent_status_identity(&previous.common).as_ref(),
        )
        && same_agent_status_occupant(&previous.common, &next.common);
    if !carried {
        return false;
    }
    match delivery.kind {
        AgentNotificationKind::Blocked => next.common.state == AgentRuntimeState::Blocked,
        AgentNotificationKind::Done => {
            next.common.state == AgentRuntimeState::Idle
                && delivery.completed_revision == Some(next.common.completed_revision)
        }
    }
}
