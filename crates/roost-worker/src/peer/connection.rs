//! One authenticated browser peer's native answerer: it creates the fixed data
//! channels before applying remote SDP, binds the socket owner's ingress to
//! the packet port before any frame can arrive, verifies the browser's DTLS
//! fingerprint once connected, and retires itself on any failure rather than
//! renegotiating. Built by `peer::owner_offer`. Ports v2
//! `apps/worker/src/terminal/peer/terminal-peer-connection.ts`.

use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use roost_protocol::terminal_peer::peer::{
    TERMINAL_PEER_DATA_CHANNELS, TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS,
    TERMINAL_PEER_MAX_MESSAGE_SIZE,
};
use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use super::faults::PeerTestFaults;
use super::native::{
    NativeChannelSpec, NativePeer, NativePeerConfig, NativePeerEvent, NativePeerEvents,
    NativePeerFactory, remote_fingerprint_matches,
};
use super::packet_budget::{TerminalPeerPacketBudget, lock};
use super::packet_port::{PacketPortDeps, TerminalPeerPacketIngress, TerminalPeerPacketPort};
use super::peer_budget::TerminalPeerPacketPeerBudget;
use crate::local_terminal::{ExpectedPeer, TerminalPacketPort};

/// v2 `TerminalPeerConnectionFailureReason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionFailure {
    IceFailed,
    ConnectionSuperseded,
}

impl ConnectionFailure {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::IceFailed => "ice_failed",
            Self::ConnectionSuperseded => "connection_superseded",
        }
    }
}

/// v2 `TerminalPeerConnectionConfig`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalPeerConnectionConfig {
    pub stun_urls: Vec<String>,
    pub bind_address: Option<IpAddr>,
    pub port_range: Option<(u16, u16)>,
}

/// v2 `OpenTerminalPeerPort`: the socket owner registers the port under the
/// offer's tuple and hands back where its messages go. `None` refuses it.
pub type OpenTerminalPeerPort = Arc<
    dyn Fn(Arc<TerminalPeerPacketPort>, ExpectedPeer) -> Option<Arc<dyn TerminalPeerPacketIngress>>
        + Send
        + Sync,
>;

/// v2 `TerminalPeerConnectionDeps`.
pub struct TerminalPeerConnectionDeps {
    pub native: Arc<dyn NativePeerFactory>,
    pub peer_id: String,
    pub expected_tuple: ExpectedPeer,
    pub expected_remote_fingerprint: String,
    pub config: TerminalPeerConnectionConfig,
    pub packet_budget: TerminalPeerPacketBudget,
    pub peer_budget: TerminalPeerPacketPeerBudget,
    pub open_peer_port: OpenTerminalPeerPort,
    pub on_closed: Arc<dyn Fn(ConnectionFailure) + Send + Sync>,
    pub socket_id: String,
    /// Smoke-only; `None` for every ordinary worker.
    pub test_faults: Option<Arc<PeerTestFaults>>,
    pub runtime: Handle,
}

impl std::fmt::Debug for TerminalPeerConnectionDeps {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalPeerConnectionDeps")
            .field("socket_id", &self.socket_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Default)]
struct ConnectionState {
    answering: bool,
    answer_waiter: Option<oneshot::Sender<Result<String, ConnectionFailure>>>,
    fingerprint_verified: bool,
    closed: bool,
    closed_notified: bool,
}

/// One immutable offer/answer exchange.
pub struct TerminalPeerConnection {
    native: Arc<dyn NativePeer>,
    port: Arc<TerminalPeerPacketPort>,
    expected_remote_fingerprint: String,
    on_closed: Arc<dyn Fn(ConnectionFailure) + Send + Sync>,
    runtime: Handle,
    this: Weak<TerminalPeerConnection>,
    state: Mutex<ConnectionState>,
}

impl std::fmt::Debug for TerminalPeerConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("TerminalPeerConnection")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

impl TerminalPeerConnection {
    /// The native peer, its channels and the packet port bound to the socket
    /// owner — or the reason none of it survived construction.
    pub fn new(deps: TerminalPeerConnectionDeps) -> Result<Arc<Self>, ConnectionFailure> {
        let channels = TERMINAL_PEER_DATA_CHANNELS
            .iter()
            .map(|definition| NativeChannelSpec {
                id: u16::from(definition.id),
                label: definition.label.to_owned(),
                ordered: true,
                protocol: definition.protocol.to_owned(),
            })
            .collect();
        let (native, events) = deps
            .native
            .create(NativePeerConfig {
                name: format!("roost-terminal-peer-{}", deps.peer_id),
                stun_urls: deps.config.stun_urls,
                bind_address: deps.config.bind_address,
                port_range: deps.config.port_range,
                max_message_size: TERMINAL_PEER_MAX_MESSAGE_SIZE,
                channels,
            })
            .map_err(|error| {
                tracing::warn!(%error, "a terminal peer's native connection could not be created");
                deps.peer_budget.dispose();
                ConnectionFailure::IceFailed
            })?;
        let connection = Arc::new_cyclic(|this: &Weak<Self>| {
            let closed = this.clone();
            let fatal = this.clone();
            Self {
                port: TerminalPeerPacketPort::new(PacketPortDeps {
                    socket_id: deps.socket_id,
                    native: Arc::clone(&native),
                    budget: deps.packet_budget,
                    peer_budget: deps.peer_budget,
                    on_closed: Some(Arc::new(move |_reason: &str| {
                        if let Some(connection) = closed.upgrade() {
                            connection.close(ConnectionFailure::ConnectionSuperseded);
                        }
                    })),
                    on_fatal: Some(Arc::new(move |_reason: &str| {
                        if let Some(connection) = fatal.upgrade() {
                            connection.close(ConnectionFailure::IceFailed);
                        }
                    })),
                    test_faults: deps.test_faults,
                    runtime: deps.runtime.clone(),
                }),
                native,
                expected_remote_fingerprint: deps.expected_remote_fingerprint,
                on_closed: deps.on_closed,
                runtime: deps.runtime,
                this: this.clone(),
                state: Mutex::new(ConnectionState::default()),
            }
        });
        let ingress = (deps.open_peer_port)(Arc::clone(&connection.port), deps.expected_tuple);
        let attached = ingress.is_some_and(|ingress| connection.port.attach_ingress(ingress));
        if !attached {
            tracing::warn!("the terminal peer port ingress is unavailable");
            // Closed first: the port's close hook must not report a
            // connection its owner never held (v2 `if (connection)`).
            connection.lock_state().closed = true;
            connection
                .port
                .close(0, ConnectionFailure::ConnectionSuperseded.as_str());
            connection.native.close();
            return Err(ConnectionFailure::ConnectionSuperseded);
        }
        connection
            .runtime
            .spawn(pump_native_events(Arc::downgrade(&connection), events));
        Ok(connection)
    }

    pub fn port(&self) -> &Arc<TerminalPeerPacketPort> {
        &self.port
    }

    pub fn is_closed(&self) -> bool {
        self.lock_state().closed
    }

    /// v2 `answer`: the answer once gathering completes, or at the gathering
    /// deadline if it already carries a candidate; `ice_failed` otherwise.
    pub async fn answer(
        &self,
        offer_sdp: String,
        deadline: Duration,
    ) -> Result<String, ConnectionFailure> {
        let receiver = {
            let mut state = self.lock_state();
            if state.closed || state.answering {
                return Err(ConnectionFailure::ConnectionSuperseded);
            }
            state.answering = true;
            let (waiter, receiver) = oneshot::channel();
            state.answer_waiter = Some(waiter);
            receiver
        };
        let gathering = deadline.min(Duration::from_millis(
            TERMINAL_PEER_ICE_GATHERING_DEADLINE_MS,
        ));
        if gathering.is_zero() {
            self.close(ConnectionFailure::IceFailed);
        } else {
            let answering = self.native.answer(offer_sdp, gathering);
            let connection = self.this.clone();
            self.runtime.spawn(async move {
                let answered = answering.await;
                let Some(connection) = connection.upgrade() else {
                    return;
                };
                match answered {
                    Ok(answer_sdp) if carries_candidates(&answer_sdp) => {
                        connection.finish_answer(answer_sdp)
                    }
                    _ => connection.close(ConnectionFailure::IceFailed),
                }
            });
        }
        receiver
            .await
            .unwrap_or(Err(ConnectionFailure::ConnectionSuperseded))
    }

    /// v2 `close`: rejects a pending answer, closes the port and the native
    /// peer, and tells the owner once.
    pub fn close(&self, reason: ConnectionFailure) {
        let waiter = {
            let mut state = self.lock_state();
            if state.closed {
                return;
            }
            state.closed = true;
            state.answer_waiter.take()
        };
        if let Some(waiter) = waiter {
            let _ = waiter.send(Err(reason));
        }
        self.port.close(0, reason.as_str());
        self.native.close();
        let notify = {
            let mut state = self.lock_state();
            !std::mem::replace(&mut state.closed_notified, true)
        };
        if notify {
            (self.on_closed)(reason);
        }
    }

    fn finish_answer(&self, answer_sdp: String) {
        let waiter = self.lock_state().answer_waiter.take();
        if let Some(waiter) = waiter {
            let _ = waiter.send(Ok(answer_sdp));
        }
    }

    /// v2 `installPeerCallbacks` plus the port's channel callbacks: channel
    /// events go to the port, after the fingerprint check a channel open needs.
    fn on_native_event(&self, event: NativePeerEvent) {
        match event {
            NativePeerEvent::Connected => self.verify_remote_fingerprint(),
            NativePeerEvent::Failed
            | NativePeerEvent::Closed
            | NativePeerEvent::UnsolicitedChannel => {
                self.close(ConnectionFailure::IceFailed);
            }
            NativePeerEvent::ChannelOpen(_) => {
                self.verify_remote_fingerprint();
                if !self.is_closed() {
                    self.port.on_channel_event(event);
                }
            }
            channel_event => self.port.on_channel_event(channel_event),
        }
    }

    fn verify_remote_fingerprint(&self) {
        {
            let state = self.lock_state();
            if state.closed || state.fingerprint_verified {
                return;
            }
        }
        if remote_fingerprint_matches(self.native.as_ref(), &self.expected_remote_fingerprint) {
            self.lock_state().fingerprint_verified = true;
            tracing::debug!(socket = %self.port.socket_id(), "a terminal peer's DTLS fingerprint matched its offer");
        } else {
            tracing::warn!(socket = %self.port.socket_id(), "a terminal peer's DTLS fingerprint did not match its offer");
            self.close(ConnectionFailure::IceFailed);
        }
    }

    fn lock_state(&self) -> MutexGuard<'_, ConnectionState> {
        lock(&self.state)
    }
}

fn carries_candidates(answer_sdp: &str) -> bool {
    inspect_terminal_peer_sdp(answer_sdp).is_ok_and(|metadata| metadata.candidate_count > 0)
}

async fn pump_native_events(
    connection: Weak<TerminalPeerConnection>,
    mut events: NativePeerEvents,
) {
    while let Some(event) = events.recv().await {
        let Some(connection) = connection.upgrade() else {
            return;
        };
        if connection.is_closed() {
            return;
        }
        connection.on_native_event(event);
    }
}
