//! What the client pushes outward, and in what order. Composes the existing Sync
//! service (`client::sync`, `sync::SyncState`) and the direct-carrier table
//! (`terminal::routes`); it decides WHAT goes out and opens nothing.
//!
//! Two things live here, and both are the shape of a request rather than a
//! transport. The grant exchange: a request to the coordinator, the decision of
//! whether to send it, and the facts a session list is decided from. The
//! terminal path: a route election, and the input-route claims that ride on it.
//!
//! What it deliberately does NOT own is the Sync generation fence, which
//! `client::sync::link` already holds as `InstalledLink` and its two gates. The
//! claims below are matched on the coordinator's socket id AND the worker
//! process epoch, because a result arriving on another connection is a result
//! about a route this client no longer holds — a gate the token cannot express,
//! since `terminal::token` deliberately keeps `socket_id` out of equality.
//!
//! Ported from `apps/web/src/store/transport/sync-outbound.ts`.

use std::collections::BTreeMap;

use crate::client::local::LocalTerminalGrant;
use crate::client::local::door::{LoopbackAdmission, ROUTE_CLAIM_TIMEOUT_MS};
use crate::client::local::grants::GrantOwner;
use crate::effect::{DirectCommand, Effect};
use crate::terminal::routes::RouteRegistry;
use crate::terminal::token::{TerminalToken, TerminalTransport};

/// The Sync socket as the outbound path reads it.
///
/// A projection of `SyncState` rather than the socket itself, so the comparison
/// rules below can be stated and proved with no socket. `socket_id` is carried
/// here and NOT in the token, because the token deliberately excludes it
/// (`terminal::token`) while a claim's answer must still be matched on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncTerminalState {
    /// Which dial this is. Monotonic per client.
    pub socket_generation: u64,
    /// The coordinator's identity for this socket.
    pub socket_id: String,
    /// The worker process epoch behind it.
    pub process_epoch: String,
    /// The terminal domain generation currently in force.
    pub domain_generation: u64,
    /// Hydration is done and the coordinator accepts terminal commands.
    pub ready: bool,
}

impl SyncTerminalState {
    /// The token terminal commands on this socket carry.
    pub fn token(&self, domain_generation: u64) -> TerminalToken {
        TerminalToken::sync(
            self.socket_generation,
            &self.socket_id,
            &self.process_epoch,
            domain_generation,
        )
    }
}

/// Whether two observations name the same Sync connection: generation, the
/// coordinator's socket id, and the worker process epoch. The domain generation
/// is deliberately NOT part of it — one moving is a re-publish on one
/// connection, not a different socket.
pub fn same_sync_connection(left: &SyncTerminalState, right: &SyncTerminalState) -> bool {
    left.socket_generation == right.socket_generation
        && left.socket_id == right.socket_id
        && left.process_epoch == right.process_epoch
}

/// Where one session's next input batch goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDestination {
    /// The generation a command on this path must carry.
    pub token: TerminalToken,
    /// The route epoch to put on an input batch.
    pub worker_epoch: String,
    /// Whether the worker acknowledged an input-route capability.
    pub input_route_supported: bool,
    /// Whether a caller may close this path. Sync is the coordinator's socket
    /// and is closed by the core, never by a destination.
    pub closeable: bool,
}

/// Elect the path for one session: an elected direct route first, Sync second.
///
/// A direct route that cannot carry input REFUSES rather than falling back, which
/// is v2's shape for an unqualified peer and is the right answer for an expired
/// grant too: the carrier is gone, and silently moving a session's input
/// elsewhere would hide a route loss the rest of the client has to hear about.
pub fn destination_for_session(
    registry: &RouteRegistry,
    sync: Option<&SyncTerminalState>,
    grants: &GrantOwner,
    session_id: &str,
    session_worker_fp: Option<&str>,
    peer_qualified: bool,
    now_ms: u64,
) -> Option<InputDestination> {
    if let Some(route) = registry.route(session_id) {
        if route.token.transport == TerminalTransport::Peer && !peer_qualified {
            return None;
        }
        let grant = grants.current(route.token.worker_fp.as_deref()?)?;
        if grant.admits(session_id, now_ms).is_err() {
            return None;
        }
        let (worker_epoch, supported) =
            route_context(Some(grant), now_ms, &route.token.process_epoch);
        return Some(InputDestination {
            token: route.token.clone(),
            worker_epoch,
            input_route_supported: supported,
            closeable: true,
        });
    }
    ready_sync_destination(sync, grants, session_worker_fp, now_ms)
}

/// The Sync fallback, and only a READY one. Never an elected direct route: this
/// is the path a blocked session is handed back to when its carrier is gone, and
/// pointing it at that carrier would be the loop.
pub fn ready_sync_destination(
    sync: Option<&SyncTerminalState>,
    grants: &GrantOwner,
    session_worker_fp: Option<&str>,
    now_ms: u64,
) -> Option<InputDestination> {
    let state = sync?;
    if !state.ready {
        return None;
    }
    let grant = session_worker_fp.and_then(|worker_fp| grants.current(worker_fp));
    let (worker_epoch, supported) = route_context(grant, now_ms, &state.process_epoch);
    Some(InputDestination {
        token: state.token(state.domain_generation),
        worker_epoch,
        input_route_supported: supported,
        closeable: false,
    })
}

/// The route epoch and capability a grant supplies, or the carrier's own epoch
/// and neither capability.
///
/// A grant past its lifetime supplies NEITHER: the worker will not honour a
/// claim against a scope it has already closed, and a stale epoch on a batch is
/// refused rather than ignored, so a dead grant's epoch is worse than none.
fn route_context(
    grant: Option<&LocalTerminalGrant>,
    now_ms: u64,
    carrier_epoch: &str,
) -> (String, bool) {
    let live = grant.filter(|grant| grant.is_live(now_ms));
    let worker_epoch = live
        .map(|grant| grant.worker_epoch.clone())
        .filter(|epoch| !epoch.is_empty())
        .unwrap_or_else(|| carrier_epoch.to_string());
    let supported =
        live.is_some_and(|grant| grant.input_route_supported && !grant.worker_epoch.is_empty());
    (worker_epoch, supported)
}

/// A claim that ended without an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteClaimEnd {
    /// The request the worker never answered.
    pub request_id: String,
    /// Why it ended.
    pub reason: &'static str,
}

/// The claim refused because its request id is already pending. A second claim
/// under a live id is a caller bug, and overwriting it would orphan the first.
pub const CLAIM_ID_BUSY: &str = "terminal input route claim is already pending";

/// The claim that timed out. Released and never retried: the worker may still
/// apply one it never answered.
pub const CLAIM_TIMEOUT: &str = "terminal input route claim timed out";

/// The claims a Sync connection change ended.
pub const CLAIM_SYNC_CLOSED: &str = "terminal Sync closed";

/// The claims a credential boundary ended. Retained bytes are never replayed
/// onto a fresh authenticated transport.
pub const CLAIM_BOUNDARY: &str = "credential boundary";

/// The input-route claims this document has outstanding.
///
/// A value, not module state, because a client core that kept this at module
/// scope would be one two browser documents share.
#[derive(Debug, Default)]
pub struct RouteClaims {
    claims: BTreeMap<String, OutstandingClaim>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutstandingClaim {
    socket_id: String,
    process_epoch: String,
    armed_at_ms: u64,
}

impl RouteClaims {
    /// A set holding nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many claims are outstanding.
    pub fn outstanding(&self) -> usize {
        self.claims.len()
    }

    /// Take a claim for one input-route request, or refuse it.
    pub fn admit(
        &mut self,
        request_id: &str,
        state: &SyncTerminalState,
        now_ms: u64,
    ) -> Result<(), &'static str> {
        if self.claims.contains_key(request_id) {
            return Err(CLAIM_ID_BUSY);
        }
        self.claims.insert(
            request_id.to_string(),
            OutstandingClaim {
                socket_id: state.socket_id.clone(),
                process_epoch: state.process_epoch.clone(),
                armed_at_ms: now_ms,
            },
        );
        Ok(())
    }

    /// Settle a claim from the worker's answer, on the connection it was sent on.
    ///
    /// A result for another connection, or another request id, is dropped rather
    /// than settled: it is an answer about a route this client no longer holds.
    pub fn settle(&mut self, request_id: &str, socket_id: &str, process_epoch: &str) -> bool {
        let Some(claim) = self.claims.get(request_id) else {
            return false;
        };
        if claim.socket_id != socket_id || claim.process_epoch != process_epoch {
            return false;
        }
        self.claims.remove(request_id).is_some()
    }

    /// Release every claim whose deadline has passed.
    pub fn expire(&mut self, now_ms: u64) -> Vec<RouteClaimEnd> {
        let expired: Vec<String> = self
            .claims
            .iter()
            .filter(|(_, claim)| now_ms.saturating_sub(claim.armed_at_ms) >= ROUTE_CLAIM_TIMEOUT_MS)
            .map(|(request_id, _)| request_id.clone())
            .collect();
        self.end(&expired, CLAIM_TIMEOUT)
    }

    /// End every claim, oldest request id first. This is what a credential
    /// boundary does to them: the claims ended here were sent under a credential
    /// this document no longer holds, so their answers are owed to nothing.
    pub fn reject_all(&mut self, reason: &'static str) -> Vec<RouteClaimEnd> {
        let ids: Vec<String> = self.claims.keys().cloned().collect();
        self.end(&ids, reason)
    }

    /// End the claims that were sent on some OTHER Sync connection.
    pub fn reject_other_connections(
        &mut self,
        state: &SyncTerminalState,
        reason: &'static str,
    ) -> Vec<RouteClaimEnd> {
        let ids: Vec<String> = self
            .claims
            .iter()
            .filter(|(_, claim)| {
                claim.socket_id != state.socket_id || claim.process_epoch != state.process_epoch
            })
            .map(|(request_id, _)| request_id.clone())
            .collect();
        self.end(&ids, reason)
    }

    fn end(&mut self, request_ids: &[String], reason: &'static str) -> Vec<RouteClaimEnd> {
        request_ids
            .iter()
            .filter_map(|request_id| {
                self.claims.remove(request_id).map(|_| RouteClaimEnd {
                    request_id: request_id.clone(),
                    reason,
                })
            })
            .collect()
    }
}

/// Why a loopback write was refused before it reached the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopbackRefusal {
    /// The carrier reports no input-route capability, so there is no epoch to
    /// claim against.
    InputRouteUnsupported,
    /// The claim names an epoch this carrier does not run against.
    WorkerEpochMismatch,
}

impl LoopbackRefusal {
    /// The string a host records.
    pub const fn reason(self) -> &'static str {
        match self {
            Self::InputRouteUnsupported => "local terminal input route is unavailable",
            Self::WorkerEpochMismatch => "local terminal input route claim names another epoch",
        }
    }
}

/// Whether an admitted loopback carrier may claim an input route for an epoch.
///
/// A DECISION and not an effect, because `DirectCommand` has no input-route-claim
/// member; the frame a host puts on the wire for a granted claim is its own
/// encoding, and this is the rule that has to hold before it is worth encoding.
/// The epoch check is the point — the worker rechecks the claim's route after
/// keeper admission and refuses a stale one, so a claim naming another epoch
/// would only learn its refusal after the batch it was protecting.
pub fn loopback_may_claim_input_route(
    admission: &LoopbackAdmission,
    claimed_epoch: &str,
) -> Result<(), LoopbackRefusal> {
    if !admission.input_route_supported {
        return Err(LoopbackRefusal::InputRouteUnsupported);
    }
    if claimed_epoch != admission.token.process_epoch {
        return Err(LoopbackRefusal::WorkerEpochMismatch);
    }
    Ok(())
}

/// What a newly admitted loopback carrier needs before it can serve a session.
///
/// One baseline request per session the `Ready` named, in session-id order, so the
/// order is the same on every run and a host that performs them in sequence folds
/// the same grid every time. The view id is empty because a carrier that has just
/// authenticated has no view yet; the pane publishes one on the route it wins.
pub fn loopback_ready_effects(admission: &LoopbackAdmission) -> Vec<Effect> {
    admission
        .ready_sessions
        .iter()
        .map(|session_id| Effect::SendDirect {
            token: admission.token.clone(),
            command: DirectCommand::Resync {
                session_id: session_id.clone(),
                view_id: String::new(),
            },
        })
        .collect()
}
