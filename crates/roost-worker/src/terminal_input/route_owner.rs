//! Browser input-route fencing at the worker boundary: an authenticated
//! ingress claims an actor/session route (`route_owner::claim`, keeper-lane half
//! `route_attempt`) before any bytes reach the keeper, a writer asks
//! `is_current` for its epoch, and `session::input_write` asks again immediately
//! before the keeper write. Called by `terminal_input::port` (coordinator link)
//! and the local door. Ports `apps/worker/src/terminal/terminal-input-route-owner.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use roost_proto::TerminalInputRouteResult;
use roost_protocol::wire::brand::SessionId;
use tokio::sync::watch;

use super::route_attempt::{AttemptCancel, route_result};
use super::work_budget::TerminalInputWorkBudget;
use crate::session::control_lanes::ControlLanes;
use crate::session::input_write::TerminalWriteBudget;
use crate::session::table::SessionTable;

mod claim;
mod retire;

/// How long a retired route fences its revision before it is pruned.
pub const TERMINAL_INPUT_ROUTE_TOMBSTONE: Duration = Duration::from_secs(60);
/// The most routes (live, blocked or tombstoned) the owner holds.
pub const TERMINAL_INPUT_ROUTE_MAX_ENTRIES: usize = 8_192;
const ROUTE_IDENTIFIER_MAX_BYTES: usize = 128;
const MAX_ROUTE_REVISION: u64 = i64::MAX as u64;

/// Who is claiming: a browser device, its tab, and the connection it spoke on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteActor {
    pub device_fingerprint: String,
    pub tab_id: String,
    pub connection_id: String,
}

/// The claim fields the owner reads. v2 `TerminalInputRouteClaim`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteClaim {
    pub request_id: String,
    pub session_id: String,
    pub revision: u64,
    pub worker_epoch: String,
}

/// A claim's request budget plus the ingress's live session authority, which
/// is retained as a predicate rather than snapshotted. v2 `TerminalInputRouteClaimBudget`.
pub trait RouteClaimBudget: TerminalWriteBudget {
    fn is_session_authorized(&self) -> bool;
}

/// The clock tombstones are measured on; a test seam, as in v2.
pub type RouteClock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub(super) type RouteKey = (String, String, String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RouteStatus {
    Active,
    Blocked,
    Retired,
}

pub(super) struct PendingAttempt {
    pub(super) id: u64,
    pub(super) connection_id: String,
    pub(super) cancel: Arc<AttemptCancel>,
    pub(super) outcome: watch::Receiver<Option<TerminalInputRouteResult>>,
}

pub(super) struct RouteEntry {
    pub(super) actor: RouteActor,
    pub(super) session_id: String,
    pub(super) latest_revision: u64,
    pub(super) input_route_epoch: Option<String>,
    pub(super) status: RouteStatus,
    pub(super) retired_until: Option<Instant>,
    pub(super) latest_claim: RouteClaim,
    pub(super) latest_result: Option<TerminalInputRouteResult>,
    pub(super) pending: Option<PendingAttempt>,
}

#[derive(Default)]
pub(super) struct RouteState {
    pub(super) routes: HashMap<RouteKey, RouteEntry>,
    pub(super) revoked_devices: HashSet<String>,
    pub(super) revoke_overflow: bool,
    pub(super) disposed: bool,
    next_attempt: u64,
}

pub(super) struct RouteShared {
    pub(super) worker_epoch: String,
    pub(super) sessions: Arc<SessionTable>,
    lanes: Arc<ControlLanes>,
    work_budget: TerminalInputWorkBudget,
    pub(super) now: RouteClock,
    state: Mutex<RouteState>,
}

impl RouteShared {
    pub(super) fn lock(&self) -> MutexGuard<'_, RouteState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The channel a session lives on, or `None` for an id this worker does
    /// not hold (including one that is not a session id at all).
    pub(super) fn channel_of(&self, session_id: &str) -> Option<u16> {
        let session_id = SessionId::try_from(session_id).ok()?;
        self.sessions.channel_of(&session_id)
    }
}

/// v2 `TerminalInputRouteOwner`. Cheap to clone; clones share one route table.
#[derive(Clone)]
pub struct TerminalInputRouteOwner {
    shared: Arc<RouteShared>,
}

impl std::fmt::Debug for TerminalInputRouteOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.lock();
        formatter
            .debug_struct("TerminalInputRouteOwner")
            .field("worker_epoch", &self.shared.worker_epoch)
            .field("routes", &state.routes.len())
            .field("disposed", &state.disposed)
            .finish()
    }
}

impl TerminalInputRouteOwner {
    pub fn new(
        worker_epoch: String,
        sessions: Arc<SessionTable>,
        lanes: Arc<ControlLanes>,
        work_budget: TerminalInputWorkBudget,
    ) -> Self {
        Self::with_clock(
            worker_epoch,
            sessions,
            lanes,
            work_budget,
            Arc::new(Instant::now),
        )
    }

    pub fn with_clock(
        worker_epoch: String,
        sessions: Arc<SessionTable>,
        lanes: Arc<ControlLanes>,
        work_budget: TerminalInputWorkBudget,
        now: RouteClock,
    ) -> Self {
        let state = Mutex::new(RouteState::default());
        let shared = RouteShared {
            worker_epoch,
            sessions,
            lanes,
            work_budget,
            now,
            state,
        };
        Self {
            shared: Arc::new(shared),
        }
    }

    /// Whether `epoch` is the live route for this actor and session.
    pub fn is_current(&self, actor: &RouteActor, session_id: &str, epoch: &str) -> bool {
        if !valid_actor(actor) || !valid_identifier(session_id) || epoch.is_empty() {
            return false;
        }
        let state = self.shared.lock();
        if state.revoke_overflow || state.revoked_devices.contains(&actor.device_fingerprint) {
            return false;
        }
        state
            .routes
            .get(&route_key(actor, session_id))
            .is_some_and(|entry| {
                entry.status == RouteStatus::Active
                    && entry.actor.connection_id == actor.connection_id
                    && entry.input_route_epoch.as_deref() == Some(epoch)
            })
    }

    /// Whether epoch-less input may pass: never while a route exists for this
    /// actor and session, until its tombstone lease has run out.
    pub fn allows_legacy_input(&self, actor: &RouteActor, session_id: &str) -> bool {
        if !valid_actor(actor) || !valid_identifier(session_id) {
            return false;
        }
        let now = (self.shared.now)();
        let mut state = self.shared.lock();
        if state.revoke_overflow || state.revoked_devices.contains(&actor.device_fingerprint) {
            return false;
        }
        let key = route_key(actor, session_id);
        let Some(entry) = state.routes.get(&key) else {
            return true;
        };
        let expired = entry.status == RouteStatus::Retired
            && entry.pending.is_none()
            && entry.retired_until.is_some_and(|until| until <= now);
        if expired {
            state.routes.remove(&key);
        }
        expired
    }
}

/// Why a claim may not enter keeper admission, or nothing when it may.
pub(super) fn pre_admission_failure(
    channel_id: Option<u16>,
    budget: &dyn RouteClaimBudget,
) -> Option<&'static str> {
    if channel_id.is_none() || !budget.is_session_authorized() {
        return Some("terminal session is unavailable");
    }
    if !budget.is_current_connection() {
        return Some("worker connection superseded");
    }
    if budget.expired() {
        return Some("route claim budget expired");
    }
    None
}

/// Drop tombstones whose lease has run out and whose ticket has drained.
pub(super) fn prune_retired(state: &mut RouteState, now: Instant) {
    state.routes.retain(|_, entry| {
        !(entry.status == RouteStatus::Retired
            && entry.pending.is_none()
            && entry.retired_until.is_some_and(|until| until <= now))
    });
}

fn route_key(actor: &RouteActor, session_id: &str) -> RouteKey {
    (
        actor.device_fingerprint.clone(),
        actor.tab_id.clone(),
        session_id.to_owned(),
    )
}

fn valid_actor(actor: &RouteActor) -> bool {
    valid_identifier(&actor.device_fingerprint)
        && valid_identifier(&actor.tab_id)
        && valid_identifier(&actor.connection_id)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= ROUTE_IDENTIFIER_MAX_BYTES
}
