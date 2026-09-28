//! Live authorization for one direct terminal port: the port's own record, the
//! registry that says whether it is still current, and the predicates input,
//! route claims and history reads evaluate. Built by `super::sockets`
//! immediately before each piece of work, and re-evaluated by the session
//! layer after keeper admission — no grant scope is copied across an await,
//! and a closed or replaced port fails every predicate. Ports
//! `apps/worker/src/local-door/local-terminal-socket-authority.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use roost_protocol::wire::brand::SessionId;

use super::grants::LocalTerminalGrantStore;
use super::port::{ExpectedPeer, PeerTerminalPacketPort, TerminalPacketPort};
use crate::session::input_write::{TerminalWriteAuthority, TerminalWriteBudget};
use crate::session::table::SessionTable;
use crate::terminal_input::{RouteActor, RouteClaimBudget, TerminalInputRouteOwner};
use crate::uplink::TERMINAL_REQUEST_BUDGET_CAP_MS;

/// Which carrier a port is, and for a peer the tuple its offer authorized.
#[derive(Debug)]
pub(super) enum Carrier {
    Loopback(Arc<dyn TerminalPacketPort>),
    Peer {
        port: Arc<dyn PeerTerminalPacketPort>,
        expected: ExpectedPeer,
    },
}

/// What a Hello bound a port to. v2 keeps these as mutable fields on the port
/// session; here they change together under one lock.
#[derive(Debug, Clone, Default)]
pub(super) struct PortIdentity {
    pub(super) generation: u64,
    pub(super) grant_id: Option<String>,
    pub(super) device_fingerprint: Option<String>,
    pub(super) tab_id: Option<String>,
}

/// One registered direct port. v2 `LocalTerminalPortSession`.
#[derive(Debug)]
pub(super) struct PortSession {
    pub(super) carrier: Carrier,
    identity: Mutex<PortIdentity>,
    closing: AtomicBool,
}

impl PortSession {
    pub(super) fn new(carrier: Carrier) -> Arc<Self> {
        Arc::new(Self {
            carrier,
            identity: Mutex::default(),
            closing: AtomicBool::new(false),
        })
    }

    pub(super) fn port(&self) -> &dyn TerminalPacketPort {
        match &self.carrier {
            Carrier::Loopback(port) => port.as_ref(),
            Carrier::Peer { port, .. } => port.as_ref(),
        }
    }

    pub(super) fn socket_id(&self) -> &str {
        self.port().socket_id()
    }

    pub(super) fn expected_peer(&self) -> Option<&ExpectedPeer> {
        match &self.carrier {
            Carrier::Loopback(_) => None,
            Carrier::Peer { expected, .. } => Some(expected),
        }
    }

    pub(super) fn kind(&self) -> &'static str {
        match self.carrier {
            Carrier::Loopback(_) => "loopback",
            Carrier::Peer { .. } => "webrtc",
        }
    }

    pub(super) fn identity(&self) -> PortIdentity {
        self.identity_lock().clone()
    }

    pub(super) fn generation(&self) -> u64 {
        self.identity_lock().generation
    }

    /// Whether a Hello bound this port to a grant.
    pub(super) fn is_authenticated(&self) -> bool {
        self.identity_lock().grant_id.is_some()
    }

    pub(super) fn grant_id_is(&self, grant_id: &str) -> bool {
        self.identity_lock().grant_id.as_deref() == Some(grant_id)
    }

    pub(super) fn bind(&self, identity: PortIdentity) {
        *self.identity_lock() = identity;
    }

    pub(super) fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    /// Mark the port closing; true only for the caller that did so first.
    pub(super) fn begin_close(&self) -> bool {
        !self.closing.swap(true, Ordering::AcqRel)
    }

    fn identity_lock(&self) -> MutexGuard<'_, PortIdentity> {
        self.identity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Every live port by socket id: the one answer to "is this port current".
#[derive(Debug, Default)]
pub(super) struct PortRegistry {
    ports: Mutex<HashMap<String, Arc<PortSession>>>,
}

impl PortRegistry {
    /// Register a port under a socket id no live port holds.
    pub(super) fn insert(&self, session: Arc<PortSession>) -> bool {
        let mut ports = self.lock();
        let socket_id = session.socket_id().to_owned();
        if ports.contains_key(&socket_id) {
            return false;
        }
        ports.insert(socket_id, session);
        true
    }

    pub(super) fn get(&self, socket_id: &str) -> Option<Arc<PortSession>> {
        self.lock().get(socket_id).cloned()
    }

    pub(super) fn remove(&self, socket_id: &str) -> Option<Arc<PortSession>> {
        self.lock().remove(socket_id)
    }

    pub(super) fn all(&self) -> Vec<Arc<PortSession>> {
        self.lock().values().cloned().collect()
    }

    /// Whether `session` is still the port registered under its socket id.
    pub(super) fn is_current(&self, session: &Arc<PortSession>) -> bool {
        self.lock()
            .get(session.socket_id())
            .is_some_and(|live| Arc::ptr_eq(live, session))
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Arc<PortSession>>> {
        self.ports
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What every predicate reads. v2 `LocalTerminalAuthorizationDeps`.
#[derive(Debug)]
pub(super) struct AuthorizationDeps {
    pub(super) sessions: Arc<SessionTable>,
    pub(super) grants: LocalTerminalGrantStore,
    pub(super) routes: TerminalInputRouteOwner,
    pub(super) worker_epoch: String,
    pub(super) ports: PortRegistry,
}

/// The route actor a Hello bound this port to, or none before one.
pub(super) fn direct_port_actor(session: &PortSession) -> Option<RouteActor> {
    let identity = session.identity();
    Some(RouteActor {
        device_fingerprint: identity.device_fingerprint?,
        tab_id: identity.tab_id?,
        connection_id: session.socket_id().to_owned(),
    })
}

/// v2 `isDirectPortSessionAuthorized`: the port is live and current, its grant
/// is still installed for the same device and tab under an acceptable epoch,
/// and the grant covers a session this worker holds.
pub(super) fn is_direct_port_session_authorized(
    deps: &AuthorizationDeps,
    session: &Arc<PortSession>,
    session_id: &str,
) -> bool {
    let identity = session.identity();
    let (Some(grant_id), Some(device), Some(tab)) = (
        identity.grant_id,
        identity.device_fingerprint,
        identity.tab_id,
    ) else {
        return false;
    };
    if session.is_closing() || !session.port().is_open() || !deps.ports.is_current(session) {
        return false;
    }
    let Some(grant) = deps.grants.current(&grant_id) else {
        return false;
    };
    if grant.device_fingerprint != device || grant.tab_id != tab {
        return false;
    }
    let epoch_ok = match session.expected_peer() {
        Some(expected) => grant.worker_epoch == expected.worker_epoch,
        None => grant.worker_epoch.is_empty() || grant.worker_epoch == deps.worker_epoch,
    };
    epoch_ok
        && grant
            .session_ids
            .iter()
            .any(|granted| granted == session_id)
        && SessionId::try_from(session_id).is_ok_and(|id| deps.sessions.channel_of(&id).is_some())
}

/// v2 `directPortRequestBudget`: the cap from the moment the work was taken,
/// and a connection that stays current only while the port is.
#[derive(Debug, Clone)]
pub(super) struct DirectPortBudget {
    pub(super) deps: Arc<AuthorizationDeps>,
    pub(super) session: Arc<PortSession>,
    pub(super) started: Instant,
}

impl DirectPortBudget {
    pub(super) fn new(deps: &Arc<AuthorizationDeps>, session: &Arc<PortSession>) -> Self {
        Self {
            deps: Arc::clone(deps),
            session: Arc::clone(session),
            started: Instant::now(),
        }
    }
}

impl TerminalWriteBudget for DirectPortBudget {
    fn is_current_connection(&self) -> bool {
        !self.session.is_closing()
            && self.session.port().is_open()
            && self.deps.ports.is_current(&self.session)
    }

    fn expired(&self) -> bool {
        self.started.elapsed() >= Duration::from_millis(u64::from(TERMINAL_REQUEST_BUDGET_CAP_MS))
    }
}

/// A route claim's budget over a direct port: the request budget plus the
/// port's live authority for the claimed session.
#[derive(Debug, Clone)]
pub(super) struct DirectClaimBudget {
    pub(super) budget: DirectPortBudget,
    pub(super) session_id: String,
}

impl TerminalWriteBudget for DirectClaimBudget {
    fn is_current_connection(&self) -> bool {
        self.budget.is_current_connection()
    }

    fn expired(&self) -> bool {
        self.budget.expired()
    }
}

impl RouteClaimBudget for DirectClaimBudget {
    fn is_session_authorized(&self) -> bool {
        is_direct_port_session_authorized(&self.budget.deps, &self.budget.session, &self.session_id)
    }
}

/// v2 `directPortInputAuthority`: the session predicate, and the route rule —
/// a peer must name its live route epoch, loopback may write epoch-less only
/// while no route holds the actor and session.
#[derive(Debug, Clone)]
pub(super) struct DirectPortAuthority {
    pub(super) deps: Arc<AuthorizationDeps>,
    pub(super) session: Arc<PortSession>,
    pub(super) session_id: String,
    pub(super) input_route_epoch: String,
}

impl TerminalWriteAuthority for DirectPortAuthority {
    fn is_session_authorized(&self) -> bool {
        is_direct_port_session_authorized(&self.deps, &self.session, &self.session_id)
    }

    fn is_current_input_route(&self) -> bool {
        let Some(actor) = direct_port_actor(&self.session) else {
            return false;
        };
        let routes = &self.deps.routes;
        let epoch = self.input_route_epoch.as_str();
        match (self.session.expected_peer(), epoch.is_empty()) {
            (Some(_), true) => false,
            (None, true) => routes.allows_legacy_input(&actor, &self.session_id),
            (_, false) => routes.is_current(&actor, &self.session_id, epoch),
        }
    }
}
