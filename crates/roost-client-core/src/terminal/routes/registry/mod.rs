//! One document's direct-carrier registry: two connection slots per worker, one
//! elected route per session, and the promotion that swaps a candidate in.
//!
//! The candidate/active split is the whole design. A worker has exactly one
//! authority, so a third connection is displaced rather than queued, and a
//! candidate takes the active slot only once it holds a route and the current
//! active holds none — which is why retiring one connection can hand its
//! sessions to the survivor with no renegotiation.
//!
//! The staged attempt itself lives in `candidate`: the registration and
//! election rules are read when a connection arrives, and the staging rules are
//! read when a frame does.
//!
//! Ported from `apps/web/src/store/terminal-stream-transport.ts:119-391`.
//! Contract: `protocol/spec/direct-terminal.md`; the reasons are in
//! `docs/phase4-client-contract.md` §8.

mod candidate;

use std::collections::{BTreeMap, BTreeSet};

use crate::terminal::routes::{DirectCarrier, SessionRoute};
use crate::terminal::token::{TerminalToken, TerminalTransport};

pub use candidate::ConnectionRegistration;

/// A route that was elected and has since been lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LostRoute {
    /// The session it served.
    pub session_id: String,
    /// The generation it was committed on, so the caller retires THAT token and
    /// not whatever the session has moved to.
    pub token: TerminalToken,
}

/// One document's direct-carrier registry and route table.
#[derive(Debug, Default)]
pub struct RouteRegistry {
    /// Per worker: the connection currently serving routes, and the one staging.
    connections: BTreeMap<String, WorkerConnections>,
    /// The elected route per session.
    routes: BTreeMap<String, SessionRoute>,
    /// Which views want which session on which worker.
    demands: BTreeMap<String, BTreeMap<String, BTreeSet<String>>>,
    /// Staged candidates by session: the METADATA of each attempt.
    pub(crate) candidates: BTreeMap<String, crate::terminal::routes::PromotionCandidate>,
    /// The replica each staged candidate is folding into, by session.
    ///
    /// Held here rather than inside `PromotionCandidate` because a
    /// `TerminalSession` owns a chunk assembler and cannot be cloned: a candidate
    /// carrying its own replica would need a second copy of a grid that has to
    /// stay one object, and `promote` hands this one out rather than copying it.
    pub(crate) staged: BTreeMap<String, crate::terminal::session::TerminalSession>,
}

/// The two connection slots one worker has.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct WorkerConnections {
    /// The connection serving elected routes.
    active: Option<DirectCarrier>,
    /// The connection proving itself.
    candidate: Option<DirectCarrier>,
}

impl RouteRegistry {
    /// A registry with no connections.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a connection, and report what the registration displaced.
    ///
    /// A worker has one active and one candidate slot. A third connection is
    /// refused while the current candidate already holds routes: displacing a
    /// candidate that is SERVING would drop a live session's carrier for a
    /// newcomer that has proved nothing yet. A connection that IS accepted may
    /// still displace a candidate that held nothing, and that candidate's staged
    /// attempts are cancelled here so their worker is told to release the views
    /// they had published.
    pub fn register(&mut self, connection: DirectCarrier) -> ConnectionRegistration {
        if connection.token.socket_generation == 0 || !connection.presents(&connection.token) {
            return ConnectionRegistration::refused();
        }
        // Both reads happen BEFORE anything moves: `entry(..)` holds a mutable
        // borrow of `self.connections`, a route read needs `&self`, and the
        // final assignment moves the connection.
        let worker_fp = connection.worker_fp.clone();
        let incumbent = self.connections.get(&worker_fp);
        if incumbent.is_some_and(|slots| {
            slots.active.as_ref() == Some(&connection)
                || slots.candidate.as_ref() == Some(&connection)
        }) {
            return ConnectionRegistration::admitted();
        }
        if incumbent.is_some_and(|slots| self.has_routes_for(&slots.candidate)) {
            return ConnectionRegistration::refused();
        }
        let displaced = incumbent.and_then(|slots| slots.candidate.as_ref());
        let displaced_id = displaced.map(|carrier| carrier.connection_id.clone());
        let cancelled = match &displaced_id {
            Some(connection_id) => self.cancel_candidates_for_connection(connection_id),
            None => Vec::new(),
        };
        self.connections.entry(worker_fp).or_default().candidate = Some(connection);
        ConnectionRegistration {
            accepted: true,
            cancelled,
        }
    }

    /// Remove a connection and report every route it was serving.
    pub fn unregister(&mut self, connection_id: &str) -> Vec<LostRoute> {
        let mut served_by = None;
        for (worker, slots) in self.connections.iter_mut() {
            if slots
                .active
                .as_ref()
                .is_some_and(|carrier| carrier.connection_id == connection_id)
            {
                slots.active = None;
                served_by = Some(worker.clone());
            }
            if slots
                .candidate
                .as_ref()
                .is_some_and(|carrier| carrier.connection_id == connection_id)
            {
                slots.candidate = None;
            }
        }
        let Some(worker_fp) = served_by else {
            return Vec::new();
        };
        let lost = self.take_routes_for_connection(connection_id);
        self.promote_candidate_if_possible(&worker_fp);
        // The views' demand outlives the connection: it is what lets the carrier
        // that replaces this one be promoted. Only `retire_worker` drops it.
        if self
            .connections
            .get(&worker_fp)
            .is_some_and(|slots| slots.active.is_none() && slots.candidate.is_none())
        {
            self.connections.remove(&worker_fp);
        }
        lost
    }

    /// The host's connection id for one worker's carrier of a given kind.
    ///
    /// Used to name the connection a staged candidate belongs to, so a promotion
    /// can refuse a candidate whose connection the registry cannot find. Active
    /// is searched before candidate, because an active connection is the one a
    /// live session is already being served by.
    pub fn route_connection_for(
        &self,
        worker_fp: &str,
        transport: TerminalTransport,
    ) -> Option<String> {
        let slots = self.connections.get(worker_fp)?;
        slots
            .active
            .iter()
            .chain(slots.candidate.iter())
            .find(|carrier| carrier.transport == transport)
            .map(|carrier| carrier.connection_id.clone())
    }

    /// Widen every connection to `worker_fp` on `process_epoch` to a fresh
    /// grant's scope, and report what each one newly admits.
    ///
    /// A grant may only ADD (v2 `TerminalPeerConnection.updateGrant`): a
    /// connection whose admitted sessions the grant does not all name keeps its
    /// scope, because the worker is still carrying those PTYs over it.
    pub fn widen_grant(
        &mut self,
        worker_fp: &str,
        process_epoch: &str,
        sessions: &BTreeSet<String>,
    ) -> Vec<(DirectCarrier, Vec<String>)> {
        let Some(slots) = self.connections.get_mut(worker_fp) else {
            return Vec::new();
        };
        let mut widened = Vec::new();
        for carrier in slots.active.iter_mut().chain(slots.candidate.iter_mut()) {
            let other_epoch =
                !process_epoch.is_empty() && carrier.token.process_epoch != process_epoch;
            if other_epoch || !carrier.granted_sessions.is_subset(sessions) {
                continue;
            }
            let added: Vec<String> = sessions
                .difference(&carrier.granted_sessions)
                .cloned()
                .collect();
            if added.is_empty() {
                continue;
            }
            carrier.granted_sessions.extend(added.iter().cloned());
            widened.push((carrier.clone(), added));
        }
        widened
    }

    /// The exact sessions the connection presenting `token` may carry.
    ///
    /// The one copy of a carrier's scope: the host's send path asks here rather
    /// than keeping its own, which a widened grant would leave behind.
    pub fn granted_sessions_for(&self, token: &TerminalToken) -> Option<&BTreeSet<String>> {
        self.carrier_presenting(token)
            .map(|carrier| &carrier.granted_sessions)
    }

    /// The registered connection presenting exactly `token`.
    pub fn carrier_presenting(&self, token: &TerminalToken) -> Option<&DirectCarrier> {
        let slots = self.connections.get(token.worker_fp.as_deref()?)?;
        slots
            .active
            .iter()
            .chain(slots.candidate.iter())
            .find(|carrier| carrier.token == *token)
    }

    /// Record that a view wants a session on a worker. Returns true when the
    /// demand actually changed, so a host can skip a no-op republish.
    pub fn set_view_demand(
        &mut self,
        worker_fp: &str,
        session_id: &str,
        view_id: &str,
        active: bool,
    ) -> bool {
        let sessions = self.demands.entry(worker_fp.to_string()).or_default();
        let views = sessions.entry(session_id.to_string()).or_default();
        let changed = if active {
            views.insert(view_id.to_string())
        } else {
            views.remove(view_id)
        };
        if views.is_empty() {
            sessions.remove(session_id);
        }
        if sessions.is_empty() {
            self.demands.remove(worker_fp);
        }
        changed
    }

    /// Whether any live view wants this session on this worker.
    pub fn has_view_demand(&self, worker_fp: &str, session_id: &str) -> bool {
        self.demands
            .get(worker_fp)
            .and_then(|sessions| sessions.get(session_id))
            .is_some_and(|views| !views.is_empty())
    }

    /// The connection staging a session, if any.
    pub fn candidate(
        &self,
        session_id: &str,
    ) -> Option<&crate::terminal::routes::PromotionCandidate> {
        self.candidates.get(session_id)
    }

    /// The elected route for a session, if any.
    pub fn route(&self, session_id: &str) -> Option<&SessionRoute> {
        self.routes.get(session_id)
    }

    /// Whether `token` still names a session's elected route.
    pub fn route_matches(&self, session_id: &str, token: &TerminalToken) -> bool {
        self.routes
            .get(session_id)
            .is_some_and(|route| &route.token == token)
    }

    /// Retire one session's route, and only that token's.
    ///
    /// Retiring a token the session has already moved off is a no-op, so a slow
    /// callback cannot retire the route a promotion just committed.
    pub fn retire_route(&mut self, session_id: &str, token: &TerminalToken) -> bool {
        if !self.route_matches(session_id, token) {
            return false;
        }
        let worker_fp = self
            .routes
            .remove(session_id)
            .and_then(|route| route.token.worker_fp.clone());
        self.candidates.remove(session_id);
        self.staged.remove(session_id);
        if let Some(worker_fp) = worker_fp {
            self.promote_candidate_if_possible(&worker_fp);
        }
        true
    }

    /// Retire everything for a worker: its connections, its routes, its demand,
    /// and its staged candidates.
    pub fn retire_worker(&mut self, worker_fp: &str) -> Vec<LostRoute> {
        let lost: Vec<LostRoute> = self
            .routes
            .iter()
            .filter(|(_, route)| route.token.worker_fp.as_deref() == Some(worker_fp))
            .map(|(session_id, route)| LostRoute {
                session_id: session_id.clone(),
                token: route.token.clone(),
            })
            .collect();
        for route in &lost {
            self.routes.remove(&route.session_id);
        }
        self.connections.remove(worker_fp);
        self.demands.remove(worker_fp);
        self.candidates
            .retain(|_, candidate| candidate.token.worker_fp.as_deref() != Some(worker_fp));
        self.staged.retain(|_, replica| {
            replica
                .generation()
                .and_then(|t| t.worker_fp.clone())
                .as_deref()
                != Some(worker_fp)
        });
        lost
    }

    /// Every session this connection was serving.
    fn take_routes_for_connection(&mut self, connection_id: &str) -> Vec<LostRoute> {
        let served: Vec<String> = self
            .routes
            .iter()
            .filter(|(_, route)| route.connection_id == connection_id)
            .map(|(session_id, _)| session_id.clone())
            .collect();
        served
            .into_iter()
            .filter_map(|session_id| {
                self.routes.remove(&session_id).map(|route| LostRoute {
                    session_id,
                    token: route.token,
                })
            })
            .collect()
    }

    fn has_routes_for(&self, carrier: &Option<DirectCarrier>) -> bool {
        let Some(carrier) = carrier else {
            return false;
        };
        self.routes
            .values()
            .any(|route| route.connection_id == carrier.connection_id)
    }

    /// A candidate takes the active slot only when it holds a route and the
    /// current active holds none.
    fn promote_candidate_if_possible(&mut self, worker_fp: &str) {
        let Some(slots) = self.connections.get(worker_fp) else {
            return;
        };
        if !self.has_routes_for(&slots.candidate) || self.has_routes_for(&slots.active) {
            return;
        }
        if let Some(slots) = self.connections.get_mut(worker_fp) {
            slots.active = slots.candidate.take();
        }
    }
}
