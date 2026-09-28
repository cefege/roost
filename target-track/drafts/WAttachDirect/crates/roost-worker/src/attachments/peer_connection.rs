//! One attachment peer's native answerer: it creates only the two attachment
//! channels and their packet port, verifies the browser's DTLS fingerprint,
//! and never touches a terminal peer. Built by `peer_owner` over the shared
//! native driver in `crate::peer::native`. Ports
//! `apps/worker/src/attachments/attachment-peer-connection.ts`.

use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use roost_protocol::attachment_transfer::{
    PACKET_MAX_BYTES, PEER_DATA_CHANNELS, PEER_ICE_GATHERING_DEADLINE_MS, PeerErrorReason,
};
use roost_protocol::terminal_peer::sdp::inspect_terminal_peer_sdp;
use tokio::sync::{mpsc, oneshot};

use super::peer_budget::AttachmentPeerPacketPeerBudget;
use super::peer_packet_port::{AttachmentPeerIngress, AttachmentPeerPacketPort};
use super::transfer_admission::AttachmentPeerExpectedTuple;
use super::transfer_port::AttachmentTransferPort;
use crate::peer::native::{
    NativeChannelSpec, NativePeer, NativePeerConfig, NativePeerEvent, NativePeerEvents,
    NativePeerFactory, remote_fingerprint_matches,
};
use crate::session::ids::mint_uuid;

/// Why a connection ended, as the owner reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentPeerConnectionFailure {
    IceFailed,
    ConnectionSuperseded,
}

impl AttachmentPeerConnectionFailure {
    pub fn as_str(self) -> &'static str {
        self.reason().as_str()
    }

    pub fn reason(self) -> PeerErrorReason {
        match self {
            Self::IceFailed => PeerErrorReason::IceFailed,
            Self::ConnectionSuperseded => PeerErrorReason::ConnectionSuperseded,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AttachmentPeerConnectionConfig {
    pub stun_urls: Vec<String>,
    pub bind_address: Option<IpAddr>,
    pub port_range: Option<(u16, u16)>,
}

/// Hands a new port to the direct receiver; `None` is a refusal.
pub type OpenAttachmentPeerPort = Arc<
    dyn Fn(
            Arc<AttachmentPeerPacketPort>,
            AttachmentPeerExpectedTuple,
        ) -> Option<Arc<dyn AttachmentPeerIngress>>
        + Send
        + Sync,
>;

/// Told once, whichever path closed the connection.
pub type ConnectionClosed = Box<dyn FnOnce(AttachmentPeerConnectionFailure) + Send>;

pub struct AttachmentPeerConnectionDeps {
    pub factory: Arc<dyn NativePeerFactory>,
    pub peer_id: String,
    pub expected_tuple: AttachmentPeerExpectedTuple,
    pub expected_remote_fingerprint: String,
    pub config: AttachmentPeerConnectionConfig,
    pub packet_budget: AttachmentPeerPacketPeerBudget,
    pub open_peer_port: OpenAttachmentPeerPort,
    pub on_closed: ConnectionClosed,
}

#[derive(Default)]
struct ConnectionState {
    closed: bool,
    answering: bool,
    fingerprint_verified: bool,
    answer_waiter: Option<oneshot::Sender<Result<String, AttachmentPeerConnectionFailure>>>,
    on_closed: Option<ConnectionClosed>,
}

struct ConnectionInner {
    native: Arc<dyn NativePeer>,
    port: Arc<AttachmentPeerPacketPort>,
    expected_remote_fingerprint: String,
    state: Mutex<ConnectionState>,
}

/// One immutable attachment offer/answer exchange. Clone shares it.
#[derive(Clone)]
pub struct AttachmentPeerConnection {
    inner: Arc<ConnectionInner>,
}

impl std::fmt::Debug for AttachmentPeerConnection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentPeerConnection")
            .field("port", &self.inner.port.socket_id())
            .field("closed", &self.is_closed())
            .finish()
    }
}

impl AttachmentPeerConnection {
    pub fn new(
        deps: AttachmentPeerConnectionDeps,
    ) -> Result<Self, AttachmentPeerConnectionFailure> {
        let channels = PEER_DATA_CHANNELS
            .iter()
            .map(|channel| NativeChannelSpec {
                id: channel.id,
                label: channel.label.to_owned(),
                ordered: channel.ordered,
                protocol: channel.protocol.to_owned(),
            })
            .collect();
        let config = NativePeerConfig {
            name: format!("roost-attachment-peer-{}", deps.peer_id),
            stun_urls: deps.config.stun_urls,
            bind_address: deps.config.bind_address,
            port_range: deps.config.port_range,
            max_message_size: PACKET_MAX_BYTES,
            channels,
        };
        let (native, events) = deps
            .factory
            .create(config)
            .map_err(|_| AttachmentPeerConnectionFailure::IceFailed)?;
        let Ok(socket_id) = mint_uuid() else {
            native.close();
            return Err(AttachmentPeerConnectionFailure::ConnectionSuperseded);
        };
        let (closed_tx, closed_rx) = mpsc::unbounded_channel();
        let budget = deps.packet_budget;
        let port = AttachmentPeerPacketPort::new(
            socket_id,
            Arc::clone(&native),
            budget.clone(),
            closed_tx,
        );
        let ingress = (deps.open_peer_port)(Arc::clone(&port), deps.expected_tuple);
        if !ingress.is_some_and(|ingress| port.attach_ingress(ingress)) {
            port.close(
                None,
                AttachmentPeerConnectionFailure::ConnectionSuperseded.as_str(),
            );
            budget.dispose();
            native.close();
            return Err(AttachmentPeerConnectionFailure::ConnectionSuperseded);
        }
        let inner = Arc::new(ConnectionInner {
            native,
            port,
            expected_remote_fingerprint: deps.expected_remote_fingerprint,
            state: Mutex::new(ConnectionState {
                on_closed: Some(deps.on_closed),
                ..ConnectionState::default()
            }),
        });
        tokio::spawn(drive_connection(Arc::clone(&inner), events, closed_rx));
        Ok(Self { inner })
    }

    pub fn is_closed(&self) -> bool {
        self.inner.lock().closed
    }

    /// The answer SDP once gathering completes, or at `min(3 s, deadline)`
    /// with whatever candidates exist; no candidate at all is ICE failure.
    pub async fn answer(
        &self,
        offer_sdp: String,
        deadline: Duration,
    ) -> Result<String, AttachmentPeerConnectionFailure> {
        let answer = {
            let mut state = self.inner.lock();
            if state.closed || state.answering {
                return Err(AttachmentPeerConnectionFailure::ConnectionSuperseded);
            }
            state.answering = true;
            let (waiter, answer) = oneshot::channel();
            state.answer_waiter = Some(waiter);
            answer
        };
        let gathering = deadline.min(Duration::from_millis(PEER_ICE_GATHERING_DEADLINE_MS));
        if gathering.is_zero() {
            self.inner.close(AttachmentPeerConnectionFailure::IceFailed);
        } else {
            let inner = Arc::clone(&self.inner);
            tokio::spawn(async move {
                let answered = inner.native.answer(offer_sdp, gathering).await;
                inner.finish_answer(answered.ok());
            });
        }
        answer
            .await
            .unwrap_or(Err(AttachmentPeerConnectionFailure::ConnectionSuperseded))
    }

    pub fn close(&self, reason: AttachmentPeerConnectionFailure) {
        self.inner.close(reason);
    }
}

impl ConnectionInner {
    fn lock(&self) -> MutexGuard<'_, ConnectionState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn finish_answer(&self, answer: Option<String>) {
        let with_candidates = answer.filter(|sdp| {
            inspect_terminal_peer_sdp(sdp).is_ok_and(|metadata| metadata.candidate_count > 0)
        });
        let Some(answer) = with_candidates else {
            self.close(AttachmentPeerConnectionFailure::IceFailed);
            return;
        };
        let waiter = {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.answer_waiter.take()
        };
        if let Some(waiter) = waiter {
            let _answered = waiter.send(Ok(answer));
        }
    }

    fn close(&self, reason: AttachmentPeerConnectionFailure) {
        let (waiter, on_closed) = {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            (state.answer_waiter.take(), state.on_closed.take())
        };
        if let Some(waiter) = waiter {
            let _rejected = waiter.send(Err(reason));
        }
        self.port.close(None, reason.as_str());
        self.native.close();
        tracing::info!(
            reason = reason.as_str(),
            "attachment peer connection closed"
        );
        if let Some(on_closed) = on_closed {
            on_closed(reason);
        }
    }

    fn verify_remote_fingerprint(&self) {
        {
            let state = self.lock();
            if state.closed || state.fingerprint_verified {
                return;
            }
        }
        if remote_fingerprint_matches(self.native.as_ref(), &self.expected_remote_fingerprint) {
            self.lock().fingerprint_verified = true;
            tracing::info!("attachment peer fingerprint verified");
        } else {
            self.close(AttachmentPeerConnectionFailure::IceFailed);
        }
    }

    fn apply_event(&self, event: NativePeerEvent) {
        let lane_of = |index: usize| PEER_DATA_CHANNELS.get(index).map(|channel| channel.lane);
        match event {
            NativePeerEvent::Connected => self.verify_remote_fingerprint(),
            NativePeerEvent::Failed
            | NativePeerEvent::Closed
            | NativePeerEvent::UnsolicitedChannel => {
                self.close(AttachmentPeerConnectionFailure::IceFailed);
            }
            NativePeerEvent::ChannelOpen(index) => match lane_of(index) {
                Some(lane) => self.port.channel_opened(lane),
                None => self.close(AttachmentPeerConnectionFailure::IceFailed),
            },
            NativePeerEvent::ChannelMessage {
                channel,
                binary,
                data,
            } => match lane_of(channel) {
                Some(lane) => self.port.channel_message(lane, binary, &data),
                None => self.close(AttachmentPeerConnectionFailure::IceFailed),
            },
            NativePeerEvent::BufferedAmountLow(_) => self.port.buffered_amount_low(),
            NativePeerEvent::ChannelClosed(_) => self.port.channel_failed("data_channel_closed"),
            NativePeerEvent::ChannelError(_) => self.port.channel_failed("data_channel_error"),
        }
    }
}

/// The connection's own task: native events in order, and the port's close
/// delivered to the ingress here rather than inside whichever call closed it.
async fn drive_connection(
    inner: Arc<ConnectionInner>,
    mut events: NativePeerEvents,
    mut port_closed: mpsc::UnboundedReceiver<String>,
) {
    loop {
        tokio::select! {
            biased;
            closed = port_closed.recv() => {
                if let Some(ingress) = inner.port.take_ingress() {
                    ingress.on_close();
                }
                tracing::debug!(reason = closed.as_deref().unwrap_or(""), "attachment peer port retired");
                inner.close(AttachmentPeerConnectionFailure::ConnectionSuperseded);
                return;
            }
            event = events.recv() => match event {
                Some(event) => inner.apply_event(event),
                None => inner.close(AttachmentPeerConnectionFailure::IceFailed),
            },
        }
    }
}
