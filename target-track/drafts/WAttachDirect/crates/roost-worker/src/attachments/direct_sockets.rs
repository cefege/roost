//! The one process-owned direct attachment receiver: protobuf admission for
//! loopback sockets and authenticated WebRTC packet ports, bound to the
//! dedicated attachment grant store; the operation owner owns file state.
//! Built by `direct_owners`, driven by `direct_loopback` and the peer ingress.
//! Ports `apps/worker/src/attachments/attachment-direct-socket.ts`.

use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame;
use roost_proto::AttachmentTransferClientFrame;
use roost_proto::buffa::Message;
use roost_protocol::attachment_transfer::{
    MAX_ACTIVE_PER_WORKER, PeerChannelLane, TransferErrorReason,
};

use super::direct_hello::{arm_hello_deadline, handle_grant_change};
use super::direct_session::{AttachmentPortSession, DirectState, Refusal};
use super::grants::{AttachmentGrantStore, GrantChange, GrantSubscription};
use super::transfer_admission::AttachmentPeerExpectedTuple;
use super::transfer_port::{AttachmentTransferPort, PortKind};
use super::upload::AttachmentOperations;

/// The asynchronous half of one received frame: a destination write and the
/// acknowledgement that follows it. The synchronous half has already run.
pub type DirectWrite = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Which lane a frame arrived on. Loopback carries every frame on one socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectLane {
    Loopback,
    Peer(PeerChannelLane),
}

impl DirectLane {
    fn carries_control(self) -> bool {
        matches!(self, Self::Loopback | Self::Peer(PeerChannelLane::Control))
    }

    fn carries_data(self) -> bool {
        matches!(self, Self::Loopback | Self::Peer(PeerChannelLane::Data))
    }
}

#[derive(Debug)]
pub struct AttachmentDirectSocketsDeps {
    pub grants: Arc<AttachmentGrantStore>,
    pub operations: AttachmentOperations,
    pub worker_fingerprint: String,
    pub worker_epoch: String,
}

#[derive(Debug)]
pub(super) struct Inner {
    pub(super) deps: AttachmentDirectSocketsDeps,
    state: Mutex<DirectState>,
    grant_subscription: Mutex<Option<GrantSubscription>>,
}

impl Inner {
    pub(super) fn lock(&self) -> MutexGuard<'_, DirectState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Clone shares one receiver.
#[derive(Clone, Debug)]
pub struct AttachmentDirectSockets {
    pub(super) inner: Arc<Inner>,
}

impl AttachmentDirectSockets {
    pub fn new(deps: AttachmentDirectSocketsDeps) -> Self {
        let inner = Arc::new(Inner {
            deps,
            state: Mutex::new(DirectState::default()),
            grant_subscription: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        let subscription = inner
            .deps
            .grants
            .subscribe(Box::new(move |change: &GrantChange| {
                if let Some(inner) = weak.upgrade() {
                    handle_grant_change(&inner, change);
                }
            }));
        *inner
            .grant_subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(subscription);
        Self { inner }
    }

    /// A loopback socket before its hello; it fills only the unauthenticated
    /// bucket, never an admitted upload's slot.
    pub fn open_loopback_port(&self, port: Arc<dyn AttachmentTransferPort>) {
        let mut state = self.inner.lock();
        let unauthenticated = state.count_sessions(|session| !session.holds_admitted_slot());
        if state.disposed || unauthenticated >= MAX_ACTIVE_PER_WORKER {
            drop(state);
            port.close(Some(1013), "attachment transfer capacity is full");
            return;
        }
        let serial = next_serial(&mut state);
        let socket_id = port.socket_id().to_owned();
        let mut session = AttachmentPortSession::new(Arc::clone(&port), serial, None);
        if port.kind() == PortKind::Loopback {
            session.setup_timer = Some(arm_hello_deadline(&self.inner, socket_id.clone(), serial));
        }
        state.sessions.insert(socket_id, session);
        tracing::info!(
            carrier = "loopback",
            active = state.sessions.len(),
            "attachment socket opened"
        );
    }

    /// A coordinator-negotiated peer port; false when capacity refused it.
    pub fn open_peer_port(
        &self,
        port: Arc<dyn AttachmentTransferPort>,
        expected_peer: AttachmentPeerExpectedTuple,
    ) -> bool {
        let mut state = self.inner.lock();
        let admitted = state.count_sessions(AttachmentPortSession::holds_admitted_slot);
        if state.disposed || admitted >= MAX_ACTIVE_PER_WORKER {
            drop(state);
            port.close(Some(1013), "attachment transfer capacity is full");
            return false;
        }
        let serial = next_serial(&mut state);
        let session = AttachmentPortSession::new(Arc::clone(&port), serial, Some(expected_peer));
        state.sessions.insert(port.socket_id().to_owned(), session);
        tracing::info!(active = state.sessions.len(), "attachment peer port opened");
        true
    }

    /// Runs every check that needs no write now, in receive order; the write,
    /// when there is one, is returned for the caller to await or spawn.
    pub fn receive_frame(
        &self,
        socket_id: &str,
        lane: DirectLane,
        bytes: &[u8],
    ) -> Option<DirectWrite> {
        let mut state = self.inner.lock();
        let operations = &self.inner.deps.operations;
        let session = state.sessions.get(socket_id)?;
        if session.terminal {
            return None;
        }
        let serial = session.serial;
        let frame = AttachmentTransferClientFrame::decode_from_slice(bytes)
            .ok()
            .and_then(|decoded| decoded.frame);
        let Some(frame) = frame else {
            state.fail(
                operations,
                socket_id,
                serial,
                Refusal::bare(TransferErrorReason::InvalidHello),
            );
            return None;
        };
        if session.metadata.is_none() {
            let hello = match frame {
                Frame::Hello(hello) if lane.carries_control() => hello,
                _ => {
                    state.fail(
                        operations,
                        socket_id,
                        serial,
                        Refusal::bare(TransferErrorReason::InvalidHello),
                    );
                    return None;
                }
            };
            let expected_peer = session.expected_peer.clone();
            drop(state);
            self.accept_hello(socket_id, serial, &hello, expected_peer.as_ref());
            return None;
        }
        if session.write_pending {
            state.fail(
                operations,
                socket_id,
                serial,
                Refusal::bare(TransferErrorReason::ChunkOutOfOrder),
            );
            return None;
        }
        match frame {
            Frame::StatusRequest(request) if lane.carries_control() => {
                state.accept_status_request(operations, socket_id, serial, &request);
                None
            }
            Frame::Chunk(chunk) if lane.carries_data() => {
                let (ticket, write) = state.begin_chunk(operations, socket_id, serial, *chunk)?;
                drop(state);
                let inner = Arc::clone(&self.inner);
                Some(Box::pin(async move {
                    let outcome = inner.deps.operations.accept_direct_chunk(write).await;
                    inner
                        .lock()
                        .settle_chunk(&inner.deps.operations, &ticket, outcome);
                }))
            }
            _ => {
                state.fail(
                    operations,
                    socket_id,
                    serial,
                    Refusal::bare(TransferErrorReason::UploadMismatch),
                );
                None
            }
        }
    }

    /// The carrier closed; a live upload releases only its descriptor, and its
    /// durable status stays queryable.
    pub fn close_port(&self, socket_id: &str) {
        let mut state = self.inner.lock();
        let Some(serial) = state.sessions.get(socket_id).map(|session| session.serial) else {
            return;
        };
        let Some(session) = state.remove_session(socket_id, serial) else {
            return;
        };
        if session.metadata.is_some() && !session.terminal {
            self.inner.deps.operations.detach_direct_carrier(socket_id);
        }
        let carrier = session.port.kind().as_str();
        tracing::info!(
            carrier,
            active = state.sessions.len(),
            "attachment socket closed"
        );
    }

    /// Explicit device revocation closes an admitted port even after its
    /// grant expired, which a grant change alone cannot reach.
    pub fn revoke_device(&self, device_fingerprint: &str) {
        let mut state = self.inner.lock();
        let revoked: Vec<(String, u64)> = state
            .sessions
            .iter()
            .filter(|(_, session)| {
                session
                    .metadata
                    .as_ref()
                    .is_some_and(|metadata| metadata.device_fingerprint == device_fingerprint)
            })
            .map(|(socket_id, session)| (socket_id.clone(), session.serial))
            .collect();
        for (socket_id, serial) in revoked {
            let refusal = Refusal::bare(TransferErrorReason::GrantUnavailable);
            state.fail(&self.inner.deps.operations, &socket_id, serial, refusal);
        }
    }

    pub fn dispose(&self) {
        let subscription = self
            .inner
            .grant_subscription
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let mut state = self.inner.lock();
        if state.disposed {
            return;
        }
        state.disposed = true;
        if let Some(subscription) = subscription {
            subscription.cancel();
        }
        let live: Vec<(String, u64)> = state
            .sessions
            .iter()
            .map(|(socket_id, session)| (socket_id.clone(), session.serial))
            .collect();
        for (socket_id, serial) in live {
            let Some(session) = state.remove_session(&socket_id, serial) else {
                continue;
            };
            if session.metadata.is_some() && !session.terminal {
                self.inner.deps.operations.detach_direct_carrier(&socket_id);
            }
            session.port.close(Some(1001), "worker disposed");
        }
        tracing::info!("attachment direct sockets disposed");
    }
}

fn next_serial(state: &mut DirectState) -> u64 {
    state.next_serial += 1;
    state.next_serial
}
