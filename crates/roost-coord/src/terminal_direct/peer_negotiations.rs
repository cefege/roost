//! Bounded browser-to-worker terminal-peer signaling: admission, the owner's
//! state, and one negotiation run from authorization to an offer on an exact
//! worker generation and back; `peer_settle` is how the offer ends. Session
//! authority stays with the grant owner; SDP is never retained or logged.
//! Built once on `terminal_direct::TerminalDirectRuntime`. Ports
//! `apps/coord/src/terminal/direct/terminal-peer-negotiations.ts`.

use std::num::NonZeroU64;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use connectrpc::ConnectError;
use roost_proto::SessionsNegotiateLocalTerminalPeerRequest as PeerRequest;
use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::terminal_direct::grant_state::{TerminalGrantInvalidation, TerminalGrantLeaseSnapshot};
use crate::terminal_direct::peer_state::{
    PeerKey, TerminalGrantSessionAuthorizer, TerminalPeerCaller, TerminalPeerGrantPort,
    TerminalPeerSettings, assert_terminal_peer_request_shape, terminal_peer_cancelled,
    terminal_peer_deadline_exceeded, terminal_peer_denied, terminal_peer_invalid,
    terminal_peer_unavailable,
};
use crate::terminal_direct::peer_table::{
    NegotiationTable, PeerOutcome, PendingTerminalPeerNegotiation, TerminalPeerAdmission,
};
use crate::workers::terminal_peer_send::{TerminalPeerOfferSend, send_terminal_peer_offer};

/// What a negotiation owner is built over.
pub struct TerminalPeerNegotiationsOptions {
    /// The process's worker registry.
    pub workers: Arc<WorkerRegistry>,
    /// The lease owner (a fake in tests).
    pub grants: Arc<dyn TerminalPeerGrantPort>,
    /// Whether the carrier is offered, and its STUN servers.
    pub settings: TerminalPeerSettings,
    /// Durable route authority over a worker's sessions.
    pub authorize_sessions: TerminalGrantSessionAuthorizer,
    /// How long a reserved offer waits for its typed answer.
    pub answer_timeout_ms: NonZeroU64,
}

impl std::fmt::Debug for TerminalPeerNegotiationsOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalPeerNegotiationsOptions")
            .field("settings", &self.settings)
            .field("answer_timeout_ms", &self.answer_timeout_ms)
            .finish_non_exhaustive()
    }
}

/// Composition-owned signaling admission and typed-answer correlation.
pub struct TerminalPeerNegotiations {
    pub(super) workers: Arc<WorkerRegistry>,
    pub(super) grants: Arc<dyn TerminalPeerGrantPort>,
    pub(super) settings: TerminalPeerSettings,
    authorize_sessions: TerminalGrantSessionAuthorizer,
    answer_timeout: Duration,
    table: Mutex<NegotiationTable>,
    pub(super) grant_subscription: Option<u64>,
    this: Weak<Self>,
}

impl std::fmt::Debug for TerminalPeerNegotiations {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalPeerNegotiations")
            .field("settings", &self.settings)
            .field("pending", &self.table().pending_count())
            .finish_non_exhaustive()
    }
}

/// One caller's negotiation: refused at admission, or running to its answer.
#[derive(Debug)]
pub struct PendingPeerNegotiation {
    task: Result<JoinHandle<PeerOutcome>, ConnectError>,
}

impl PendingPeerNegotiation {
    /// The worker's validated answer, or why there is none.
    pub async fn response(self) -> PeerOutcome {
        match self.task {
            Err(error) => Err(error),
            Ok(task) => task.await.unwrap_or_else(|_| Err(signaling_unavailable())),
        }
    }
}

/// An admission's capacity charge, released however its negotiation ends
/// before it becomes a pending offer.
struct AdmissionHold {
    owner: Arc<TerminalPeerNegotiations>,
    key: Option<PeerKey>,
}

impl Drop for AdmissionHold {
    fn drop(&mut self) {
        if let Some(key) = self.key.take() {
            self.owner.table().release_admission(&key);
        }
    }
}

impl TerminalPeerNegotiations {
    /// An owner subscribed to its grant port's invalidations.
    #[must_use]
    pub fn new(options: TerminalPeerNegotiationsOptions) -> Arc<Self> {
        Arc::new_cyclic(|this: &Weak<Self>| {
            let listener_owner = this.clone();
            let grant_subscription = options.grants.subscribe_invalidation(Arc::new(
                move |invalidation: &TerminalGrantInvalidation| {
                    if let Some(owner) = listener_owner.upgrade() {
                        owner.cancel_invalidated(invalidation);
                    }
                },
            ));
            Self {
                workers: options.workers,
                grants: options.grants,
                settings: options.settings,
                authorize_sessions: options.authorize_sessions,
                answer_timeout: Duration::from_millis(options.answer_timeout_ms.get()),
                table: Mutex::new(NegotiationTable::default()),
                grant_subscription,
                this: this.clone(),
            }
        })
    }

    /// Whether the carrier is offered, and its STUN servers.
    #[must_use]
    pub fn settings(&self) -> &TerminalPeerSettings {
        &self.settings
    }

    /// Admit a negotiation NOW (v2 runs `negotiate` synchronously up to its
    /// first await), then run it to the worker's answer on its own task, so a
    /// dropped caller still keeps its admission charged until authorization
    /// exits. `abort` is the browser's cancellation.
    pub fn negotiate(
        &self,
        caller: TerminalPeerCaller,
        authenticated_tab_id: Option<&str>,
        request: PeerRequest,
        abort: CancellationToken,
    ) -> PendingPeerNegotiation {
        let task = self
            .admit(&caller, authenticated_tab_id, &request, &abort)
            .map(|(hold, offer_digest, grant)| {
                let owner = Arc::clone(&hold.owner);
                tokio::spawn(owner.run_admitted(caller, request, abort, hold, offer_digest, grant))
            });
        PendingPeerNegotiation { task }
    }

    fn admit(
        &self,
        caller: &TerminalPeerCaller,
        authenticated_tab_id: Option<&str>,
        request: &PeerRequest,
        abort: &CancellationToken,
    ) -> Result<(AdmissionHold, String, TerminalGrantLeaseSnapshot), ConnectError> {
        assert_terminal_peer_request_shape(request)?;
        let owner = self.this.upgrade().ok_or_else(signaling_unavailable)?;
        if self.table().disposed {
            return Err(signaling_unavailable());
        }
        if authenticated_tab_id != Some(request.tab_id.as_str()) {
            return Err(terminal_peer_denied(
                "terminal peer tab does not match the authenticated document",
            ));
        }
        if abort.is_cancelled() {
            return Err(terminal_peer_cancelled());
        }
        let offer_digest = hex::encode(Sha256::digest(request.offer_sdp.as_bytes()));
        let key = (
            caller.owner_key.clone(),
            request.tab_id.clone(),
            request.worker_fp.clone(),
        );
        let admission = TerminalPeerAdmission {
            peer_id: request.peer_id.clone(),
            offer_digest: offer_digest.clone(),
            device_fingerprint: caller.device_fingerprint.clone(),
            worker_fp: request.worker_fp.clone(),
        };
        self.table().claim_admission(key.clone(), admission)?;
        let hold = AdmissionHold {
            owner,
            key: Some(key),
        };
        inspect_terminal_peer_sdp(&request.offer_sdp)
            .map_err(|_| terminal_peer_invalid("terminal peer offer is invalid"))?;
        let grant = self.require_owned_grant(&caller.owner_key, request)?;
        Ok((hold, offer_digest, grant))
    }

    async fn run_admitted(
        self: Arc<Self>,
        caller: TerminalPeerCaller,
        request: PeerRequest,
        abort: CancellationToken,
        mut hold: AdmissionHold,
        offer_digest: String,
        grant: TerminalGrantLeaseSnapshot,
    ) -> PeerOutcome {
        let owner_key = caller.owner_key.as_str();
        (self.authorize_sessions)(request.worker_fp.clone(), grant.session_ids.clone()).await?;
        if self.table().disposed {
            return Err(signaling_unavailable());
        }
        if abort.is_cancelled() {
            return Err(terminal_peer_cancelled());
        }
        let stable = self.require_stable_grant(owner_key, &request, &grant)?;
        let worker = self.require_worker(&request, &stable)?;
        let (request_id, deadline, settled) =
            self.reserve(&caller, &request, &stable, &worker, offer_digest, &mut hold)?;
        drop(hold);
        if abort.is_cancelled() {
            self.cancel_pending(&request_id, terminal_peer_cancelled(), "aborted", false);
            return settled
                .await
                .unwrap_or_else(|_| Err(signaling_unavailable()));
        }
        let budget_ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis();
        if budget_ms == 0 {
            self.cancel_pending(
                &request_id,
                terminal_peer_deadline_exceeded(),
                "timeout",
                false,
            );
            return settled
                .await
                .unwrap_or_else(|_| Err(signaling_unavailable()));
        }
        let offer = TerminalPeerOfferSend {
            request_id: request_id.clone(),
            grant_id: stable.grant_id.clone(),
            peer_id: request.peer_id.clone(),
            device_fingerprint: caller.device_fingerprint.clone(),
            tab_id: request.tab_id.clone(),
            worker_epoch: request.worker_epoch.clone(),
            offer_sdp: request.offer_sdp.clone(),
            budget_ms: u32::try_from(budget_ms).unwrap_or(u32::MAX),
            stun_urls: self.settings.stun_urls.clone(),
        };
        if !send_terminal_peer_offer(&self.workers, &worker, offer) {
            let error = terminal_peer_unavailable("terminal peer worker is unavailable");
            self.cancel_pending(&request_id, error, "send_failed", false);
            return settled
                .await
                .unwrap_or_else(|_| Err(signaling_unavailable()));
        }
        tracing::debug!(worker_fp = %request.worker_fp, pending = self.table().pending_count(),
            "terminal peer negotiations: offer_sent");
        let response = self
            .await_answer(&request_id, deadline, &abort, settled)
            .await?;
        let current = self.require_stable_grant(owner_key, &request, &stable)?;
        (self.authorize_sessions)(request.worker_fp.clone(), current.session_ids.clone()).await?;
        let latest = self.require_stable_grant(owner_key, &request, &current)?;
        self.require_worker(&request, &latest)?;
        Ok(response)
    }

    /// Turn the admission into a pending offer on the exact worker generation.
    fn reserve(
        &self,
        caller: &TerminalPeerCaller,
        request: &PeerRequest,
        grant: &TerminalGrantLeaseSnapshot,
        worker: &Arc<WorkerHandle>,
        offer_digest: String,
        hold: &mut AdmissionHold,
    ) -> Result<(String, Instant, oneshot::Receiver<PeerOutcome>), ConnectError> {
        let mut table = self.table();
        if table.disposed {
            return Err(signaling_unavailable());
        }
        let request_id = table.allocate_request_id()?;
        let key = hold.key.take().ok_or_else(signaling_unavailable)?;
        let (settle, settled) = oneshot::channel();
        table.reserve(
            &key,
            PendingTerminalPeerNegotiation {
                request_id: request_id.clone(),
                owner_key: caller.owner_key.clone(),
                device_fingerprint: caller.device_fingerprint.clone(),
                tab_id: request.tab_id.clone(),
                worker_fp: request.worker_fp.clone(),
                grant_id: grant.grant_id.clone(),
                peer_id: request.peer_id.clone(),
                worker: Arc::clone(worker),
                connection_generation: worker.connection_generation.clone(),
                worker_epoch: request.worker_epoch.clone(),
                offer_digest,
                settle,
            },
        );
        Ok((request_id, Instant::now() + self.answer_timeout, settled))
    }

    pub(super) fn table(&self) -> MutexGuard<'_, NegotiationTable> {
        self.table.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(super) fn signaling_unavailable() -> ConnectError {
    terminal_peer_unavailable("terminal peer signaling is unavailable")
}
