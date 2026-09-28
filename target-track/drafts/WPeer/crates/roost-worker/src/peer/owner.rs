//! The lifecycle owner for authenticated native browser terminal peers: it
//! loads the transport once, reserves a bounded number of peers, answers one
//! offer per peer, and retires only the peer a cancel, revoke, detach or close
//! names. Built by `peer::direct::DirectTerminal`; offers arrive from the
//! direct-terminal downstream arms. Ports v2
//! `apps/worker/src/terminal/peer/terminal-peer-owner.ts` (the offer path is
//! `peer::owner_offer`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use roost_proto::DLocalTerminalPeerCancel;
use roost_proto::DLocalTerminalPeerOffer;
use tokio::runtime::Handle;
use tokio::sync::OnceCell;

use super::config::PeerTransportConfig;
use super::connection::{OpenTerminalPeerPort, TerminalPeerConnection, TerminalPeerConnectionConfig};
use super::faults::OfferFaultSlot;
use super::native::{NativeLoader, NativePeerFactory};
use super::packet_budget::{TerminalPeerPacketBudget, lock};
use crate::local_terminal::{ExpectedPeer, PeerGrantAuthorization, TerminalPacketPort};
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// v2 `TerminalPeerBootstrapState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerBootstrapState {
    Disabled,
    NativeUnavailable,
    Ready,
}

/// v2 `TerminalPeerOfferFailureReason`: the `reason` a refused offer's
/// `local-terminal-peer-error` carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalPeerOfferFailure {
    Disabled,
    NativeUnavailable,
    InvalidOffer,
    GrantUnavailable,
    Capacity,
    Expired,
    ConnectionSuperseded,
    IceFailed,
}

impl TerminalPeerOfferFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NativeUnavailable => "native_unavailable",
            Self::InvalidOffer => "invalid_offer",
            Self::GrantUnavailable => "grant_unavailable",
            Self::Capacity => "capacity",
            Self::Expired => "expired",
            Self::ConnectionSuperseded => "connection_superseded",
            Self::IceFailed => "ice_failed",
        }
    }
}

/// v2 `TerminalPeerOwnerDeps`.
pub struct TerminalPeerOwnerDeps {
    pub process_epoch: String,
    pub transport: PeerTransportConfig,
    /// Is this the exact coordinator connection generation that delivered it?
    pub is_current_coordinator: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    /// The live grant scope, without its digest or secret.
    pub authorize_grant: Arc<dyn Fn(&DLocalTerminalPeerOffer) -> PeerGrantAuthorization + Send + Sync>,
    pub open_peer_port: OpenTerminalPeerPort,
    pub native_loader: NativeLoader,
    pub packet_budget: TerminalPeerPacketBudget,
    /// Smoke-only; `None` for every ordinary worker.
    pub offer_faults: Option<Arc<OfferFaultSlot>>,
    /// Removes a grant as expired, for the `expired_grant` fault.
    pub expire_grant: Arc<dyn Fn(&str) + Send + Sync>,
    pub runtime: Handle,
}

impl std::fmt::Debug for TerminalPeerOwnerDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TerminalPeerOwnerDeps").field("transport", &self.transport).finish_non_exhaustive()
    }
}

/// An offer between admission and its answer (v2 `PendingPeer`). The token is
/// v2's object identity: a replaced or cancelled entry never matches it.
#[derive(Debug)]
pub(super) struct PendingPeer {
    pub(super) token: u64,
    pub(super) request: DLocalTerminalPeerOffer,
    pub(super) budget: RequestBudget,
    pub(super) fence: LinkFence,
    pub(super) expected: ExpectedPeer,
    pub(super) config: TerminalPeerConnectionConfig,
    pub(super) connection: Option<Arc<TerminalPeerConnection>>,
}

#[derive(Debug)]
pub(super) struct ActivePeer {
    pub(super) expected: ExpectedPeer,
    pub(super) connection: Arc<TerminalPeerConnection>,
}

#[derive(Debug, Default)]
pub(super) struct OwnerState {
    pub(super) pending: HashMap<String, PendingPeer>,
    pub(super) active: HashMap<String, ActivePeer>,
    pub(super) next_token: u64,
    pub(super) bootstrap_logged: bool,
    pub(super) native: Option<Arc<dyn NativePeerFactory>>,
    pub(super) native_cleaned: bool,
    pub(super) disposed: bool,
}

/// One worker's bounded direct-peer owner. Bootstrap is explicit for
/// capability publication and lazy on offer.
pub struct TerminalPeerOwner {
    pub(super) deps: TerminalPeerOwnerDeps,
    pub(super) bootstrap: OnceCell<PeerBootstrapState>,
    pub(super) state: Mutex<OwnerState>,
}

impl std::fmt::Debug for TerminalPeerOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TerminalPeerOwner").field("deps", &self.deps).finish_non_exhaustive()
    }
}

impl TerminalPeerOwner {
    pub fn new(deps: TerminalPeerOwnerDeps) -> Arc<Self> {
        Arc::new(Self {
            deps,
            bootstrap: OnceCell::new(),
            state: Mutex::new(OwnerState::default()),
        })
    }

    pub fn established_count(&self) -> usize {
        self.state().active.len()
    }

    pub fn negotiation_count(&self) -> usize {
        self.state().pending.len()
    }

    /// v2 `bootstrap`: `disabled` without loading anything, otherwise the one
    /// shared load's outcome.
    pub fn bootstrap(self: &Arc<Self>) -> OwnerFuture<PeerBootstrapState> {
        let owner = Arc::clone(self);
        Box::pin(async move {
            if !owner.deps.transport.enabled {
                let mut state = owner.state();
                if !std::mem::replace(&mut state.bootstrap_logged, true) {
                    tracing::info!("terminal peer native transport disabled");
                }
                return PeerBootstrapState::Disabled;
            }
            if owner.state().disposed {
                return PeerBootstrapState::NativeUnavailable;
            }
            *owner.bootstrap.get_or_init(|| owner.load_native()).await
        })
    }

    async fn load_native(&self) -> PeerBootstrapState {
        match (self.deps.native_loader)().await {
            Ok(native) => {
                let mut state = self.state();
                if state.disposed {
                    drop(state);
                    self.cleanup_native(&native);
                    return PeerBootstrapState::NativeUnavailable;
                }
                state.native = Some(native);
                tracing::info!("terminal peer native transport ready");
                PeerBootstrapState::Ready
            }
            Err(error) => {
                tracing::warn!(%error, "terminal peer native transport unavailable");
                PeerBootstrapState::NativeUnavailable
            }
        }
    }

    /// v2 `cancel`: only the exact offer the coordinator cancels.
    pub fn cancel(&self, request: &DLocalTerminalPeerCancel) {
        let connection = {
            let mut state = self.state();
            let matches = state.pending.get(&request.request_id).is_some_and(|pending| {
                pending.request.connection_generation == request.connection_generation
                    && pending.request.worker_epoch == request.worker_epoch
                    && pending.request.peer_id == request.peer_id
            });
            if !matches {
                return;
            }
            let removed = state.pending.remove(&request.request_id);
            tracing::info!(pending = state.pending.len(), "terminal peer negotiation cancelled");
            removed.and_then(|pending| pending.connection)
        };
        close_all(connection, super::connection::ConnectionFailure::ConnectionSuperseded);
    }

    /// v2 `revokeDevice`: every pending and established peer of the device.
    pub fn revoke_device(&self, device_fingerprint: &str) {
        let connections: Vec<Arc<TerminalPeerConnection>> = {
            let mut state = self.state();
            let mut closing = Vec::new();
            state.pending.retain(|_, pending| {
                let revoked = pending.expected.device_fingerprint == device_fingerprint;
                if revoked {
                    closing.push(pending.connection.take());
                }
                !revoked
            });
            let pending_closed = closing.len();
            let mut connections: Vec<_> = closing.into_iter().flatten().collect();
            let before = state.active.len();
            state.active.retain(|_, active| {
                let revoked = active.expected.device_fingerprint == device_fingerprint;
                if revoked {
                    connections.push(Arc::clone(&active.connection));
                }
                !revoked
            });
            let closed = pending_closed + before - state.active.len();
            if closed > 0 {
                tracing::info!(peers = closed, "terminal peers of a revoked device closed");
            }
            connections
        };
        close_all(connections, super::connection::ConnectionFailure::ConnectionSuperseded);
    }

    /// v2 `cancelPendingForCoordinator`: unsettled signaling is fenced by
    /// coordinator loss; established peers keep their grant lifetime.
    pub fn cancel_pending_for_coordinator(&self) {
        let connections: Vec<_> = {
            let mut state = self.state();
            let cancelled: Vec<PendingPeer> = state.pending.drain().map(|(_, pending)| pending).collect();
            if !cancelled.is_empty() {
                tracing::info!(pending = cancelled.len(), "terminal peer negotiations fenced by coordinator detach");
            }
            cancelled.into_iter().filter_map(|pending| pending.connection).collect()
        };
        close_all(connections, super::connection::ConnectionFailure::ConnectionSuperseded);
    }

    /// v2 `dispose`: every peer closes, the budget stops, the transport is
    /// cleaned up once.
    pub fn dispose(&self) {
        let (active, native) = {
            let mut state = self.state();
            if std::mem::replace(&mut state.disposed, true) {
                return;
            }
            let active: Vec<_> = state.active.drain().map(|(_, active)| active.connection).collect();
            (active, state.native.clone())
        };
        self.cancel_pending_for_coordinator();
        close_all(active, super::connection::ConnectionFailure::ConnectionSuperseded);
        self.deps.packet_budget.dispose();
        if let Some(native) = native {
            self.cleanup_native(&native);
        }
        tracing::info!("terminal peer owner disposed");
    }

    /// v2 `handleConnectionClosed`: only the connection that is active under
    /// its peer id is removed.
    pub(super) fn connection_closed(&self, peer_id: &str, socket_id: &str, reason: &str) {
        let mut state = self.state();
        let current = state
            .active
            .get(peer_id)
            .is_some_and(|active| active.connection.port().socket_id() == socket_id);
        if current {
            state.active.remove(peer_id);
            tracing::info!(peers = state.active.len(), reason, "terminal peer closed");
        }
    }

    fn cleanup_native(&self, native: &Arc<dyn NativePeerFactory>) {
        let first = {
            let mut state = self.state();
            !std::mem::replace(&mut state.native_cleaned, true)
        };
        if first {
            native.cleanup();
        }
    }

    pub(super) fn state(&self) -> MutexGuard<'_, OwnerState> {
        lock(&self.state)
    }
}

/// Closes outside the owner's lock: a closing connection reports back into it.
fn close_all(
    connections: impl IntoIterator<Item = Arc<TerminalPeerConnection>>,
    reason: super::connection::ConnectionFailure,
) {
    for connection in connections {
        connection.close(reason);
    }
}
