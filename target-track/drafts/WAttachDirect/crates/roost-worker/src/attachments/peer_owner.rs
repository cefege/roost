//! The worker's bounded attachment peer owner: admission, negotiation and
//! close state independent of terminal peers, borrowing only the process-owned
//! native loader (the terminal owner stays its sole cleaner). Built by
//! `direct_owners`; offers and cancels arrive from the coordinator link. Ports
//! `apps/worker/src/attachments/attachment-peer-owner.ts`; the path from an
//! admitted offer to its answer is in `peer_negotiation`.

use std::collections::HashMap;
use std::fmt;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_proto::{
    DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer, WLocalAttachmentPeerAnswer,
};
use roost_protocol::attachment_transfer::PeerErrorReason;
use roost_protocol::terminal_peer::peer::parse_terminal_peer_stun_urls;
use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;
use tokio::sync::OnceCell;

use super::grants::PeerGrantAuthorization;
use super::peer_budget::AttachmentPeerPacketBudget;
use super::peer_connection::{
    AttachmentPeerConnection, AttachmentPeerConnectionConfig,
    AttachmentPeerConnectionFailure as Failure, OpenAttachmentPeerPort,
};
use super::peer_negotiation::{PendingPeer, admission_failure};
use super::transfer_admission::AttachmentPeerExpectedTuple;
use crate::peer::native::{NativeLoader, NativePeerFactory};
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// Whether the native runtime this owner borrows is usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentPeerBootstrapState {
    Disabled,
    NativeUnavailable,
    Ready,
}

/// Authorizes an offer's grant tuple against the attachment grant store.
pub type AuthorizeAttachmentPeerGrant =
    Arc<dyn Fn(&DLocalAttachmentPeerOffer) -> PeerGrantAuthorization + Send + Sync>;

/// v2 `isCurrentCoordinator(connectionGeneration)`.
pub type IsCurrentCoordinator = Arc<dyn Fn(&str) -> bool + Send + Sync>;

pub struct AttachmentPeerOwnerDeps {
    pub process_epoch: String,
    pub enabled: bool,
    pub bind_address: Option<IpAddr>,
    pub port_range: Option<(u16, u16)>,
    pub is_current_coordinator: IsCurrentCoordinator,
    pub authorize_grant: AuthorizeAttachmentPeerGrant,
    pub open_peer_port: OpenAttachmentPeerPort,
    pub native_loader: NativeLoader,
    pub packet_budget: AttachmentPeerPacketBudget,
}

impl fmt::Debug for AttachmentPeerOwnerDeps {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentPeerOwnerDeps")
            .field("process_epoch", &self.process_epoch)
            .field("enabled", &self.enabled)
            .field("bind_address", &self.bind_address)
            .field("port_range", &self.port_range)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
pub(super) struct ActivePeer {
    pub expected_tuple: AttachmentPeerExpectedTuple,
    pub connection: AttachmentPeerConnection,
    pub serial: u64,
}

#[derive(Default)]
pub(super) struct OwnerState {
    pub pending: HashMap<String, PendingPeer>,
    pub active: HashMap<String, ActivePeer>,
    pub native: Option<Arc<dyn NativePeerFactory>>,
    bootstrap_state: Option<AttachmentPeerBootstrapState>,
    pub disposed: bool,
    pub next_serial: u64,
}

pub(super) struct OwnerInner {
    pub deps: AttachmentPeerOwnerDeps,
    state: Mutex<OwnerState>,
    bootstrap: OnceCell<AttachmentPeerBootstrapState>,
}

impl OwnerInner {
    pub fn lock(&self) -> MutexGuard<'_, OwnerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Takes the offer's pending entry if it is still the one `serial` made.
    pub fn take_pending(&self, request_id: &str, serial: u64) -> Option<PendingPeer> {
        let mut state = self.lock();
        let ours = state
            .pending
            .get(request_id)
            .is_some_and(|pending| pending.serial == serial);
        if ours {
            state.pending.remove(request_id)
        } else {
            None
        }
    }
}

/// Clone shares one owner.
#[derive(Clone)]
pub struct AttachmentPeerOwner {
    pub(super) inner: Arc<OwnerInner>,
}

impl fmt::Debug for AttachmentPeerOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.inner.lock();
        formatter
            .debug_struct("AttachmentPeerOwner")
            .field("pending", &state.pending.len())
            .field("active", &state.active.len())
            .finish_non_exhaustive()
    }
}

impl AttachmentPeerOwner {
    pub fn new(deps: AttachmentPeerOwnerDeps) -> Self {
        let state = Mutex::new(OwnerState::default());
        Self {
            inner: Arc::new(OwnerInner {
                deps,
                state,
                bootstrap: OnceCell::new(),
            }),
        }
    }

    /// `None` until the first bootstrap settles.
    pub fn capability_state(&self) -> Option<AttachmentPeerBootstrapState> {
        self.inner.lock().bootstrap_state
    }

    pub fn established_count(&self) -> usize {
        self.inner.lock().active.len()
    }

    /// Loads the shared native runtime once; concurrent callers share the load.
    pub async fn bootstrap(&self) -> AttachmentPeerBootstrapState {
        if !self.inner.deps.enabled {
            let mut state = self.inner.lock();
            if state.bootstrap_state.is_none() {
                state.bootstrap_state = Some(AttachmentPeerBootstrapState::Disabled);
                tracing::info!("attachment peer native runtime disabled");
            }
            return AttachmentPeerBootstrapState::Disabled;
        }
        if self.inner.lock().disposed {
            return AttachmentPeerBootstrapState::NativeUnavailable;
        }
        *self
            .inner
            .bootstrap
            .get_or_init(|| self.load_native())
            .await
    }

    /// Admission, offer inspection and the pending reservation run NOW, in
    /// the frame's receive order, so a cancel that follows finds the entry;
    /// the returned future negotiates.
    pub fn offer(
        &self,
        request: DLocalAttachmentPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Result<WLocalAttachmentPeerAnswer, PeerErrorReason>> {
        let admitted = self.admit_offer(&request, budget, fence);
        let owner = self.clone();
        Box::pin(async move {
            let (serial, config, fingerprint) = admitted?;
            let negotiated = owner
                .negotiate(&request, serial, config, fingerprint, budget)
                .await;
            negotiated.inspect_err(|reason| owner.refuse_offer(&request, serial, *reason))
        })
    }

    fn admit_offer(
        &self,
        request: &DLocalAttachmentPeerOffer,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> Result<(u64, AttachmentPeerConnectionConfig, String), PeerErrorReason> {
        if let Some(failure) = admission_failure(&self.inner, request, &budget, &fence) {
            return Err(failure);
        }
        let stun_list_valid = request.stun_urls.len() <= 4
            && request
                .stun_urls
                .iter()
                .all(|url| !url.is_empty() && !url.contains(','));
        let fingerprint = inspect_terminal_peer_sdp(&request.offer_sdp).ok();
        let stun_urls = parse_terminal_peer_stun_urls(Some(&request.stun_urls.join(","))).ok();
        let (true, Some(fingerprint), Some(stun_urls)) = (stun_list_valid, fingerprint, stun_urls)
        else {
            return Err(PeerErrorReason::InvalidOffer);
        };
        let serial = self.admit_pending(request, budget, fence)?;
        let config = AttachmentPeerConnectionConfig {
            stun_urls,
            bind_address: self.inner.deps.bind_address,
            port_range: self.inner.deps.port_range,
        };
        Ok((serial, config, fingerprint.fingerprint_sha256))
    }

    /// A negotiation that failed after its reservation releases it and closes
    /// whatever connection it had begun.
    fn refuse_offer(
        &self,
        request: &DLocalAttachmentPeerOffer,
        serial: u64,
        reason: PeerErrorReason,
    ) {
        let pending = self.inner.take_pending(&request.request_id, serial);
        if let Some(connection) = pending.and_then(|pending| pending.connection) {
            connection.close(if reason == PeerErrorReason::IceFailed {
                Failure::IceFailed
            } else {
                Failure::ConnectionSuperseded
            });
        }
        let pending = self.inner.lock().pending.len();
        tracing::warn!(
            reason = reason.as_str(),
            pending,
            "attachment peer offer refused"
        );
    }

    pub fn cancel(&self, request: &DLocalAttachmentPeerCancel) {
        let connection = {
            let mut state = self.inner.lock();
            let matches = state
                .pending
                .get(&request.request_id)
                .is_some_and(|pending| {
                    pending.request.connection_generation == request.connection_generation
                        && pending.request.worker_epoch == request.worker_epoch
                        && pending.request.peer_id == request.peer_id
                });
            if !matches {
                return;
            }
            let pending = state.pending.remove(&request.request_id);
            tracing::info!(pending = state.pending.len(), "attachment peer cancelled");
            pending.and_then(|pending| pending.connection)
        };
        if let Some(connection) = connection {
            connection.close(Failure::ConnectionSuperseded);
        }
    }

    pub fn revoke_device(&self, device_fingerprint: &str) {
        let mut closing: Vec<Option<AttachmentPeerConnection>> = Vec::new();
        {
            let mut state = self.inner.lock();
            state.pending.retain(|_, pending| {
                let revoked = pending.expected_tuple.device_fingerprint == device_fingerprint;
                if revoked {
                    closing.push(pending.connection.take());
                }
                !revoked
            });
            state.active.retain(|_, active| {
                let revoked = active.expected_tuple.device_fingerprint == device_fingerprint;
                if revoked {
                    closing.push(Some(active.connection.clone()));
                }
                !revoked
            });
        }
        let peers = closing.len();
        for connection in closing.into_iter().flatten() {
            connection.close(Failure::ConnectionSuperseded);
        }
        if peers > 0 {
            tracing::info!(peers, "attachment peer device revoked");
        }
    }

    pub fn cancel_pending_for_coordinator(&self) {
        let cancelled: Vec<PendingPeer> = self
            .inner
            .lock()
            .pending
            .drain()
            .map(|(_, pending)| pending)
            .collect();
        let pending = cancelled.len();
        for connection in cancelled
            .into_iter()
            .filter_map(|pending| pending.connection)
        {
            connection.close(Failure::ConnectionSuperseded);
        }
        if pending > 0 {
            tracing::info!(pending, "attachment peer coordinator detached");
        }
    }

    pub fn dispose(&self) {
        let active: Vec<ActivePeer> = {
            let mut state = self.inner.lock();
            if state.disposed {
                return;
            }
            state.disposed = true;
            state.active.drain().map(|(_, active)| active).collect()
        };
        self.cancel_pending_for_coordinator();
        for peer in active {
            peer.connection.close(Failure::ConnectionSuperseded);
        }
        self.inner.deps.packet_budget.dispose();
        tracing::info!("attachment peer owner disposed");
    }

    async fn load_native(&self) -> AttachmentPeerBootstrapState {
        let loaded = (self.inner.deps.native_loader)().await;
        let mut state = self.inner.lock();
        match loaded {
            Ok(_) if state.disposed => AttachmentPeerBootstrapState::NativeUnavailable,
            Ok(native) => {
                state.native = Some(native);
                state.bootstrap_state = Some(AttachmentPeerBootstrapState::Ready);
                tracing::info!("attachment peer native runtime ready");
                AttachmentPeerBootstrapState::Ready
            }
            Err(_) => {
                state.bootstrap_state = Some(AttachmentPeerBootstrapState::NativeUnavailable);
                tracing::warn!("attachment peer native runtime unavailable");
                AttachmentPeerBootstrapState::NativeUnavailable
            }
        }
    }
}
