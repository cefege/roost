//! The offer path of the terminal peer owner: the smoke fault's injection
//! site, admission (epoch, identity, coordinator generation, budget, grant,
//! SDP, capacity) decided synchronously in receive order, then bootstrap, one
//! connection and its answer, re-checked against cancellation at every step.
//! Called by `peer::direct` for each `localTerminalPeerOffer`. Ports
//! `offer`/`admissionFailure`/`assertCurrent` of v2
//! `apps/worker/src/terminal/peer/terminal-peer-owner.ts`.

use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use roost_proto::{DLocalTerminalPeerOffer, WLocalTerminalPeerAnswer};
use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER, TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER,
    TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS, TERMINAL_PEER_STUN_URL_MAX_COUNT,
    parse_terminal_peer_stun_urls,
};
use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;

use super::connection::{
    TerminalPeerConnection, TerminalPeerConnectionConfig, TerminalPeerConnectionDeps,
};
use super::connection_failure::ConnectionFailure;
use super::faults::OfferFault;
use super::owner::{
    ActivePeer, PeerBootstrapState, PendingPeer, TerminalPeerOfferFailure, TerminalPeerOwner,
};
use super::request_validation::valid_terminal_peer_offer_identity;
use crate::local_terminal::{ExpectedPeer, PeerGrantAuthorization};
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// An admitted offer, keyed back to its pending entry by `token`.
struct Admitted {
    request_id: String,
    token: u64,
    remote_fingerprint: String,
}

impl TerminalPeerOwner {
    /// v2 `offer`. Admission happens now, before the next frame is read; the
    /// future negotiates.
    pub fn offer(
        self: &Arc<Self>,
        request: DLocalTerminalPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Result<WLocalTerminalPeerAnswer, TerminalPeerOfferFailure>> {
        let admitted = self.admit(request, budget, fence);
        let owner = Arc::clone(self);
        Box::pin(async move { owner.negotiate(admitted?).await })
    }

    fn admit(
        &self,
        mut request: DLocalTerminalPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> Result<Admitted, TerminalPeerOfferFailure> {
        let fault = self
            .deps
            .test_faults
            .as_ref()
            .and_then(|faults| faults.offer().consume());
        if let Some(fault) = fault {
            tracing::info!(fault = fault.as_str(), "terminal peer offer fault applied");
            match fault {
                OfferFault::InvalidSdp => request.offer_sdp = "smoke-invalid-sdp".to_owned(),
                OfferFault::MissingGrant => request.grant_id = "smoke-missing-grant".to_owned(),
                OfferFault::ExpiredGrant => (self.deps.expire_grant)(&request.grant_id),
                OfferFault::IdentityMismatch => {}
            }
        }
        if let Some(failure) = self.admission_failure(&request, &budget, &fence) {
            return Err(failure);
        }
        let (remote_fingerprint, stun_urls) =
            inspect_offer(&request).ok_or(TerminalPeerOfferFailure::InvalidOffer)?;
        let mut state = self.state();
        let actor_busy = state
            .pending
            .values()
            .map(|pending| &pending.expected)
            .chain(state.active.values().map(|active| &active.expected))
            .any(|expected| {
                expected.device_fingerprint == request.device_fingerprint
                    && expected.tab_id == request.tab_id
            });
        if state.pending.len() >= TERMINAL_PEER_MAX_NEGOTIATIONS_PER_WORKER
            || state.active.len() + state.pending.len() >= TERMINAL_PEER_MAX_ESTABLISHED_PER_WORKER
            || state.pending.contains_key(&request.request_id)
            || state.active.contains_key(&request.peer_id)
            || state
                .pending
                .values()
                .any(|pending| pending.request.peer_id == request.peer_id)
            || actor_busy
        {
            return Err(TerminalPeerOfferFailure::Capacity);
        }
        let worker_epoch = if fault == Some(OfferFault::IdentityMismatch) {
            format!("{}-smoke-mismatch", request.worker_epoch)
        } else {
            request.worker_epoch.clone()
        };
        let expected = ExpectedPeer {
            peer_id: request.peer_id.clone(),
            grant_id: request.grant_id.clone(),
            device_fingerprint: request.device_fingerprint.clone(),
            tab_id: request.tab_id.clone(),
            worker_epoch,
        };
        state.next_token += 1;
        let token = state.next_token;
        let request_id = request.request_id.clone();
        let config = TerminalPeerConnectionConfig {
            stun_urls,
            bind_address: self.deps.transport.bind_address,
            port_range: self.deps.transport.port_range,
        };
        state.pending.insert(
            request_id.clone(),
            PendingPeer {
                token,
                request,
                budget,
                fence,
                expected,
                config,
                connection: None,
            },
        );
        tracing::info!(pending = state.pending.len(), "terminal peer negotiating");
        Ok(Admitted {
            request_id,
            token,
            remote_fingerprint,
        })
    }

    async fn negotiate(
        self: Arc<Self>,
        admitted: Admitted,
    ) -> Result<WLocalTerminalPeerAnswer, TerminalPeerOfferFailure> {
        let mut connection = None;
        let failure = match self.negotiate_connection(&admitted, &mut connection).await {
            Ok(answer) => return Ok(answer),
            Err(failure) => failure,
        };
        if let Some(connection) = connection {
            let reason = if failure == TerminalPeerOfferFailure::IceFailed {
                ConnectionFailure::IceFailed
            } else {
                ConnectionFailure::ConnectionSuperseded
            };
            connection.close(reason);
        }
        let mut state = self.state();
        if state
            .pending
            .get(&admitted.request_id)
            .is_some_and(|pending| pending.token == admitted.token)
        {
            state.pending.remove(&admitted.request_id);
        }
        tracing::warn!(
            reason = failure.as_str(),
            pending = state.pending.len(),
            "terminal peer offer refused"
        );
        Err(failure)
    }

    async fn negotiate_connection(
        self: &Arc<Self>,
        admitted: &Admitted,
        connection_slot: &mut Option<Arc<TerminalPeerConnection>>,
    ) -> Result<WLocalTerminalPeerAnswer, TerminalPeerOfferFailure> {
        let offer_received = Instant::now();
        let bootstrap = self.bootstrap().await;
        let pending = self.assert_current(admitted)?;
        let native = self.state().native.clone();
        let native = match (bootstrap, native) {
            (PeerBootstrapState::Ready, Some(native)) => native,
            (PeerBootstrapState::Disabled, _) => return Err(TerminalPeerOfferFailure::Disabled),
            _ => return Err(TerminalPeerOfferFailure::NativeUnavailable),
        };
        let peer_budget = self
            .deps
            .packet_budget
            .create_peer_budget()
            .ok_or(TerminalPeerOfferFailure::IceFailed)?;
        let socket_id = fresh_socket_id(&pending.request.peer_id);
        let owner: Weak<Self> = Arc::downgrade(self);
        let (peer_id, closed_socket) = (pending.request.peer_id.clone(), socket_id.clone());
        let connection = TerminalPeerConnection::new(TerminalPeerConnectionDeps {
            native,
            peer_id: pending.request.peer_id.clone(),
            expected_tuple: pending.expected.clone(),
            expected_remote_fingerprint: admitted.remote_fingerprint.clone(),
            config: pending.config.clone(),
            packet_budget: self.deps.packet_budget.clone(),
            peer_budget,
            open_peer_port: Arc::clone(&self.deps.open_peer_port),
            on_closed: Arc::new(move |reason: ConnectionFailure| {
                if let Some(owner) = owner.upgrade() {
                    owner.connection_closed(&peer_id, &closed_socket, reason.as_str());
                }
            }),
            socket_id,
            test_faults: self.deps.test_faults.clone(),
            runtime: self.deps.runtime.clone(),
        })?;
        *connection_slot = Some(Arc::clone(&connection));
        {
            let mut state = self.state();
            match state.pending.get_mut(&admitted.request_id) {
                Some(entry) if entry.token == admitted.token => {
                    entry.connection = Some(Arc::clone(&connection))
                }
                _ => return Err(TerminalPeerOfferFailure::ConnectionSuperseded),
            }
        }
        let deadline = native_answer_deadline(&pending.budget);
        let answer_sdp = connection
            .answer(pending.request.offer_sdp.clone(), deadline)
            .await?;
        if connection.is_closed() {
            return Err(TerminalPeerOfferFailure::IceFailed);
        }
        self.assert_current(admitted)?;
        let mut state = self.state();
        let current = state
            .pending
            .get(&admitted.request_id)
            .is_some_and(|entry| entry.token == admitted.token);
        let Some(promoted) = current
            .then(|| state.pending.remove(&admitted.request_id))
            .flatten()
        else {
            return Err(TerminalPeerOfferFailure::ConnectionSuperseded);
        };
        state.active.insert(
            promoted.request.peer_id.clone(),
            ActivePeer {
                expected: promoted.expected,
                connection: Arc::clone(&connection),
            },
        );
        let ready_ms = u64::try_from(offer_received.elapsed().as_millis()).unwrap_or(u64::MAX);
        tracing::info!(
            peers = state.active.len(),
            ready_ms,
            "terminal peer established"
        );
        Ok(WLocalTerminalPeerAnswer {
            request_id: promoted.request.request_id,
            connection_generation: promoted.request.connection_generation,
            worker_epoch: self.deps.process_epoch.clone(),
            peer_id: promoted.request.peer_id,
            answer_sdp,
            ..Default::default()
        })
    }

    /// v2 `assertCurrent`: still the pending entry this offer made, and still
    /// admissible. Returns a copy of what the next step needs.
    fn assert_current(
        &self,
        admitted: &Admitted,
    ) -> Result<PendingSnapshot, TerminalPeerOfferFailure> {
        let snapshot = {
            let state = self.state();
            let entry = state
                .pending
                .get(&admitted.request_id)
                .filter(|entry| entry.token == admitted.token)
                .ok_or(TerminalPeerOfferFailure::ConnectionSuperseded)?;
            PendingSnapshot {
                request: entry.request.clone(),
                budget: entry.budget,
                fence: entry.fence.clone(),
                expected: entry.expected.clone(),
                config: entry.config.clone(),
            }
        };
        match self.admission_failure(&snapshot.request, &snapshot.budget, &snapshot.fence) {
            Some(failure) => Err(failure),
            None => Ok(snapshot),
        }
    }

    /// v2 `admissionFailure`. Never called under the owner's lock: the grant
    /// check can retire sockets, which reports back into this owner.
    fn admission_failure(
        &self,
        request: &DLocalTerminalPeerOffer,
        budget: &RequestBudget,
        fence: &LinkFence,
    ) -> Option<TerminalPeerOfferFailure> {
        if self.state().disposed || request.worker_epoch != self.deps.process_epoch {
            return Some(TerminalPeerOfferFailure::ConnectionSuperseded);
        }
        if !self.deps.transport.enabled {
            return Some(TerminalPeerOfferFailure::Disabled);
        }
        if !valid_terminal_peer_offer_identity(request) {
            return Some(TerminalPeerOfferFailure::InvalidOffer);
        }
        if !(self.deps.is_current_coordinator)(&request.connection_generation)
            || !fence.is_current()
        {
            return Some(TerminalPeerOfferFailure::ConnectionSuperseded);
        }
        if budget.expired(Instant::now()) {
            return Some(TerminalPeerOfferFailure::IceFailed);
        }
        match (self.deps.authorize_grant)(request) {
            PeerGrantAuthorization::Authorized => None,
            PeerGrantAuthorization::Expired => Some(TerminalPeerOfferFailure::Expired),
            PeerGrantAuthorization::GrantUnavailable => {
                Some(TerminalPeerOfferFailure::GrantUnavailable)
            }
        }
    }
}

/// The pending entry's fields, read under the lock and used without it.
struct PendingSnapshot {
    request: DLocalTerminalPeerOffer,
    budget: RequestBudget,
    fence: LinkFence,
    expected: ExpectedPeer,
    config: TerminalPeerConnectionConfig,
}

/// The offer's DTLS fingerprint and its normalized STUN list, or `None` for
/// an offer v2 refuses as `invalid_offer`.
fn inspect_offer(request: &DLocalTerminalPeerOffer) -> Option<(String, Vec<String>)> {
    if request.stun_urls.len() > TERMINAL_PEER_STUN_URL_MAX_COUNT
        || request
            .stun_urls
            .iter()
            .any(|url| url.is_empty() || url.contains(','))
    {
        return None;
    }
    let fingerprint = inspect_terminal_peer_sdp(&request.offer_sdp)
        .ok()?
        .fingerprint_sha256;
    let joined = request.stun_urls.join(",");
    let stun_urls = parse_terminal_peer_stun_urls(Some(joined.as_str())).ok()?;
    Some((fingerprint, stun_urls))
}

fn native_answer_deadline(budget: &RequestBudget) -> Duration {
    budget.remaining(Instant::now()).min(Duration::from_millis(
        TERMINAL_PEER_NATIVE_ANSWER_DEADLINE_MS,
    ))
}

/// v2 `randomUUID()`: a port id no other carrier shares, minted by the one
/// uuid source the worker has.
fn fresh_socket_id(peer_id: &str) -> String {
    crate::session::ids::mint_uuid().unwrap_or_else(|error| {
        tracing::warn!(%error, "no entropy for a peer socket id; the peer id names it");
        format!("terminal-peer-{peer_id}")
    })
}
