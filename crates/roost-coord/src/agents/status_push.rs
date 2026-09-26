//! The debounced Web Push schedule for accepted agent transitions.
//!
//! Ported from `apps/coord/src/agents/agent-status-push-scheduler.ts`. The
//! status hub hands this module an ACCEPTED update and the row it displaced;
//! everything here is the decision of whether that change deserves a phone
//! notification rather than a Sync broadcast, and of when.
//!
//! WHAT IS WORTH A PUSH, AND WHY IT IS NOT "EVERYTHING". Two transitions
//! qualify: working -> blocked, because a human is now the only thing that can
//! move the agent forward, and (working|blocked) -> idle with a strictly
//! advanced `completed_revision`, because a completed TURN is what a person
//! wants to know about. Every other update is already on the wire to every
//! connected client, so a push for it would be a second copy of something the
//! screen already says. Widening this set is not a tuning knob: each state
//! added here wakes a phone for something the user is already looking at, and
//! the debounce stops being able to absorb a burst.
//!
//! NOTIFICATION IS NOT STATE: a push describes the transition that armed it,
//! not whatever the agent is doing when it lands, so every arm re-reads the
//! retained row and a superseded schedule sends nothing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use roost_observability::LogFields;
use roost_protocol::wire::agent_status::agent_status_identity;
use roost_protocol::wire::{
    AgentOccupantId, AgentRuntimeState, AgentStatus, AgentStatusUpdate, SessionId, StatusEpoch,
};

use crate::agents::status_order::same_agent_status_occupant;
use crate::push::dispatch::{
    ActiveTerminalViewers, AgentPushTransition, PushTransition, fire_push_for_transition,
};
// `push::transport`'s own type; `dispatch` imports it privately.
use crate::push::transport::PushNotificationTransport;

/// How long a transition is held before a phone is told about it: one second
/// (`agent-status-push-scheduler.ts:41`). A blocked agent republishes as its
/// message and authority source change, and a reconnect republishes a finished
/// one, so without a debounce one turn produces several notifications. It is a
/// DEBOUNCE, not a grace period: the state is broadcast the moment it is
/// accepted, so widening this window would not make the answer fresher.
pub const AGENT_STATUS_PUSH_DELAY: Duration = Duration::from_millis(1_000);

/// What the schedule must re-read to decide an armed notification is still the
/// truth: the status hub's own table, so no second copy of the fenced state.
pub trait CurrentAgentStatus: Send + Sync {
    /// The status retained for `session_id` right now, if any.
    fn current(&self, session_id: &SessionId) -> Option<AgentStatus>;
}

/// How an accepted transition reaches a phone.
///
/// The trait is the seam because the coordinator has no production
/// [`PushNotificationTransport`] to hand the dispatch yet, while the decision of
/// WHICH transitions are worth one belongs here and not in the push domain.
pub trait AgentStatusPushDelivery: Send + Sync {
    /// Whether this deployment can deliver a push at all. An empty operator
    /// allowlist is not "quiet", it is switched off, and the schedule must not
    /// arm work that can only be dropped.
    fn is_enabled(&self) -> bool;

    /// Deliver `transition`, re-checking `is_current` as the work proceeds.
    fn deliver(
        &self,
        transition: &AgentPushTransition,
        is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    );
}

/// The production delivery: the push dispatch, over the coordinator's own
/// collaborators. Constructed once at boot and installed on the schedule.
pub struct PushTransitions {
    pool: sqlx::SqlitePool,
    allowed_origins: Vec<String>,
    viewers: Arc<dyn ActiveTerminalViewers>,
    transport: Arc<dyn PushNotificationTransport>,
}

impl PushTransitions {
    /// A delivery over the pool, the operator's origin allowlist, the terminal
    /// viewers (so a device watching the terminal is not told twice), and the
    /// Web Push transport.
    #[must_use]
    pub fn new(
        pool: sqlx::SqlitePool,
        allowed_origins: Vec<String>,
        viewers: Arc<dyn ActiveTerminalViewers>,
        transport: Arc<dyn PushNotificationTransport>,
    ) -> Self {
        Self {
            pool,
            allowed_origins,
            viewers,
            transport,
        }
    }
}

impl AgentStatusPushDelivery for PushTransitions {
    fn is_enabled(&self) -> bool {
        !self.allowed_origins.is_empty()
    }

    fn deliver(
        &self,
        transition: &AgentPushTransition,
        is_current: Arc<dyn Fn() -> bool + Send + Sync>,
    ) {
        let pool = self.pool.clone();
        let allowed_origins = self.allowed_origins.clone();
        let viewers = Arc::clone(&self.viewers);
        let transport = Arc::clone(&self.transport);
        let transition = transition.clone();
        tokio::spawn(async move {
            fire_push_for_transition(
                &pool,
                &transition,
                &allowed_origins,
                viewers.as_ref(),
                &is_current,
                transport.as_ref(),
            )
            .await;
        });
    }
}

impl std::fmt::Debug for PushTransitions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PushTransitions")
            .field("allowed_origins", &self.allowed_origins)
            .finish_non_exhaustive()
    }
}

/// One accepted transition, waiting out the debounce.
#[derive(Debug, Clone)]
pub struct PendingPush {
    /// The session the notification is about; the armed table is keyed by it.
    session_id: SessionId,
    status_epoch: StatusEpoch,
    occupant_id: AgentOccupantId,
    /// The revision that CAUSED the transition, kept across a same-state
    /// re-arm so a notification is never attributed to a later republish.
    trigger_revision: i64,
    /// The revision the retained row must still be at for this to be sent.
    current_revision: i64,
    kind: PushTransition,
    /// Distinguishes this arm from the next one for the same session, so a
    /// superseded task settling late cannot deliver the newer one early.
    generation: u64,
}

#[derive(Default)]
struct ArmedTable {
    pending: HashMap<SessionId, PendingPush>,
    next_generation: u64,
}

struct ScheduleState {
    armed: Mutex<ArmedTable>,
    delivery: Mutex<Option<Arc<dyn AgentStatusPushDelivery>>>,
    delay: Duration,
}

/// The coordinator's push schedule: at most one armed transition per session.
///
/// `Clone` shares, because the task that settles an arm must read the same armed
/// table the hub writes; a schedule that cloned its table would find nothing
/// armed when it woke and would never notify anybody.
#[derive(Clone)]
pub struct AgentStatusPushSchedule {
    state: Arc<ScheduleState>,
}

impl std::fmt::Debug for AgentStatusPushSchedule {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AgentStatusPushSchedule")
            .field("armed", &self.lock(&self.state.armed).pending.len())
            .field("delay_ms", &self.state.delay.as_millis())
            .finish()
    }
}

impl AgentStatusPushSchedule {
    /// A schedule with no delivery installed.
    #[must_use]
    pub fn new(delay: Duration) -> Self {
        Self {
            state: Arc::new(ScheduleState {
                armed: Mutex::new(ArmedTable::default()),
                delivery: Mutex::new(None),
                delay,
            }),
        }
    }

    /// Install the process-wide delivery. Called once at boot.
    pub fn install_delivery(&self, delivery: Arc<dyn AgentStatusPushDelivery>) {
        *self.lock(&self.state.delivery) = Some(delivery);
    }

    /// Arm, carry forward, or drop the notification this update implies.
    pub fn arm(
        &self,
        current: Arc<dyn CurrentAgentStatus>,
        previous: Option<&AgentStatus>,
        next: &AgentStatusUpdate,
    ) {
        let Some(delivery) = self
            .lock(&self.state.delivery)
            .clone()
            .filter(|delivery| delivery.is_enabled())
        else {
            return;
        };
        let carried = self.cancel(&next.common.session_id);
        let Some(identity) = agent_status_identity(&next.common) else {
            return;
        };
        if !next.active {
            return;
        }
        let (kind, trigger_revision) = match classify_transition(previous, next) {
            Some(kind) => (kind, next.common.revision),
            None => match carried
                .as_ref()
                .filter(|carried| carried_still_holds(carried, next))
            {
                Some(carried) => (carried.kind, carried.trigger_revision),
                None => return,
            },
        };
        let pending = PendingPush {
            session_id: next.common.session_id.clone(),
            status_epoch: identity.status_epoch,
            occupant_id: identity.occupant_id,
            trigger_revision,
            current_revision: next.common.revision,
            kind,
            generation: self.next_generation(),
        };
        self.lock(&self.state.armed)
            .pending
            .insert(pending.session_id.clone(), pending.clone());
        let schedule = self.clone();
        let settle_at = tokio::time::Instant::now() + self.state.delay;
        tokio::spawn(async move {
            tokio::time::sleep_until(settle_at).await;
            schedule.settle(&current, &pending, delivery.as_ref());
        });
    }

    /// Drop any armed notification for one session, and return what it was.
    pub fn cancel(&self, session_id: &SessionId) -> Option<PendingPush> {
        self.lock(&self.state.armed).pending.remove(session_id)
    }

    /// Drop every armed notification, for a hub that is stopping.
    pub fn clear(&self) {
        self.lock(&self.state.armed).pending.clear();
    }

    fn next_generation(&self) -> u64 {
        let mut armed = self.lock(&self.state.armed);
        armed.next_generation += 1;
        armed.next_generation
    }

    /// The debounce elapsed: send only if this arm is still the armed one and
    /// the retained row still says what it said when it was armed.
    fn settle(
        &self,
        current: &Arc<dyn CurrentAgentStatus>,
        pending: &PendingPush,
        delivery: &dyn AgentStatusPushDelivery,
    ) {
        let session_id = &pending.session_id;
        let mut armed = self.lock(&self.state.armed);
        match armed.pending.get(session_id) {
            Some(held) if held.generation == pending.generation => {
                armed.pending.remove(session_id);
            }
            // Superseded while the debounce ran: the newer arm owns this
            // session now, and delivering here would notify twice.
            _ => return,
        }
        drop(armed);
        let Some(status) = current.current(session_id) else {
            return;
        };
        if !matches_current(pending, &status) {
            return;
        }
        roost_observability::log::info(
            "agents.status",
            "push_settled",
            LogFields::new()
                .set("session_id", session_id.as_str())
                .set("kind", pending.kind.as_str())
                .set("revision", pending.trigger_revision),
        );
        let transition = AgentPushTransition {
            session_id: session_id.clone(),
            kind: pending.kind,
            status_epoch: pending.status_epoch.clone(),
            occupant_id: pending.occupant_id.clone(),
            revision: u64::try_from(pending.trigger_revision).unwrap_or_default(),
        };
        let fence_pending = pending.clone();
        let fence_current = Arc::clone(current);
        // `deliver` takes a `'static` closure, so the session id is copied in
        // rather than borrowed: the browser's tab outlives this call and the
        // future must not hold a pointer into this frame.
        let fence_session_id: SessionId = session_id.clone();
        delivery.deliver(
            &transition,
            Arc::new(move || {
                fence_current
                    .current(&fence_session_id)
                    .is_some_and(|status| matches_current(&fence_pending, &status))
            }),
        );
    }

    fn lock<'q, T>(&self, mutex: &'q Mutex<T>) -> MutexGuard<'q, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Which notification, if any, the move from `previous` to `next` earns. Both
/// arms require the SAME occupant: a replacement is a different agent, and it
/// neither inherits nor triggers the notification the previous one earned.
fn classify_transition(
    previous: Option<&AgentStatus>,
    next: &AgentStatusUpdate,
) -> Option<PushTransition> {
    let previous = previous?;
    if !next.active || !same_agent_status_occupant(&previous.common, &next.common) {
        return None;
    }
    if previous.common.state == AgentRuntimeState::Working
        && next.common.state == AgentRuntimeState::Blocked
    {
        return Some(PushTransition::Blocked);
    }
    let was_active = matches!(
        previous.common.state,
        AgentRuntimeState::Working | AgentRuntimeState::Blocked
    );
    if was_active
        && next.common.state == AgentRuntimeState::Idle
        && next.common.completed_revision > previous.common.completed_revision
    {
        return Some(PushTransition::Done);
    }
    None
}

/// Whether an armed notification survives a same-state republish of its
/// occupant: the agent is still blocked, or still finished, and the update only
/// moved the revision, the message or the authority source. The source is
/// deliberately not part of this test, because a screen observation of the same
/// occupant is the same notification.
fn carried_still_holds(pending: &PendingPush, next: &AgentStatusUpdate) -> bool {
    let Some(identity) = agent_status_identity(&next.common) else {
        return false;
    };
    if identity.status_epoch != pending.status_epoch || identity.occupant_id != pending.occupant_id
    {
        return false;
    }
    match pending.kind {
        PushTransition::Blocked => next.common.state == AgentRuntimeState::Blocked,
        PushTransition::Done => next.common.state == AgentRuntimeState::Idle,
    }
}

/// Whether the retained row is still exactly the transition that was armed.
fn matches_current(pending: &PendingPush, status: &AgentStatus) -> bool {
    let Some(identity) = agent_status_identity(&status.common) else {
        return false;
    };
    if identity.status_epoch != pending.status_epoch
        || identity.occupant_id != pending.occupant_id
        || status.common.revision != pending.current_revision
    {
        return false;
    }
    let expected = match pending.kind {
        PushTransition::Blocked => AgentRuntimeState::Blocked,
        PushTransition::Done => AgentRuntimeState::Idle,
    };
    status.common.state == expected
}
