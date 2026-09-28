//! The fragmented WebRTC byte carrier for one direct attachment upload: two
//! attachment-only packet queues and reassemblers over the native peer's
//! control and data channels, and the incoming half — bounds, reassembly,
//! hello and stall deadlines, close. `direct_sockets` stays the sole protobuf
//! admission boundary. Built and driven by `peer_connection`; the outgoing
//! half is `peer_packet_egress`. Ports
//! `apps/worker/src/attachments/attachment-peer-packet-port.ts`.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use roost_protocol::attachment_transfer::{
    AttachmentTransferPacketAssembler, AttachmentTransferPacketDirection,
    AttachmentTransferPacketQueue, HELLO_DEADLINE_MS, PACKET_MAX_BYTES, PACKET_STALL_MS,
    PeerChannelLane, parse_attachment_transfer_packet,
};
use tokio::sync::mpsc;
use tokio::task::AbortHandle;

use super::peer_budget::{AttachmentPeerLaneQuota, AttachmentPeerPacketPeerBudget};
use crate::peer::native::NativePeer;

pub(super) const LANES: [PeerChannelLane; 2] = [PeerChannelLane::Control, PeerChannelLane::Data];

/// Where a reassembled frame goes: the direct receiver's view of this port.
pub trait AttachmentPeerIngress: Send + Sync + fmt::Debug {
    fn on_message(&self, lane: PeerChannelLane, bytes: Vec<u8>);
    fn on_close(&self);
}

/// The native channel index of a lane: channels are created in
/// `PEER_DATA_CHANNELS` order, control first.
pub fn lane_index(lane: PeerChannelLane) -> usize {
    match lane {
        PeerChannelLane::Control => 0,
        PeerChannelLane::Data => 1,
    }
}

pub(super) struct LaneState {
    pub queue: AttachmentTransferPacketQueue<AttachmentPeerLaneQuota>,
    pub assembler: AttachmentTransferPacketAssembler<AttachmentPeerLaneQuota>,
    pub partial_timer: Option<AbortHandle>,
}

pub(super) struct PortState {
    pub lanes: [LaneState; 2],
    pub ingress: Option<Arc<dyn AttachmentPeerIngress>>,
    pub authenticated: bool,
    pub closed: bool,
    pub hello_timer: Option<AbortHandle>,
    pub drain_timer: Option<AbortHandle>,
    pub close_when_drained: Option<String>,
}

/// Attachment-only two-channel packet transport.
pub struct AttachmentPeerPacketPort {
    pub(super) socket_id: String,
    pub(super) native: Arc<dyn NativePeer>,
    budget: AttachmentPeerPacketPeerBudget,
    origin: tokio::time::Instant,
    /// Tells the owning connection this port closed, and why; the connection
    /// delivers the ingress close from its own task, never from inside a call.
    closed_tx: mpsc::UnboundedSender<String>,
    pub(super) this: Weak<AttachmentPeerPacketPort>,
    state: Mutex<PortState>,
}

impl fmt::Debug for AttachmentPeerPacketPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentPeerPacketPort")
            .field("socket_id", &self.socket_id)
            .finish_non_exhaustive()
    }
}

impl AttachmentPeerPacketPort {
    pub fn new(
        socket_id: String,
        native: Arc<dyn NativePeer>,
        budget: AttachmentPeerPacketPeerBudget,
        closed_tx: mpsc::UnboundedSender<String>,
    ) -> Arc<Self> {
        let lane_state = |quota: AttachmentPeerLaneQuota| LaneState {
            queue: AttachmentTransferPacketQueue::new(
                AttachmentTransferPacketDirection::Outgoing,
                quota.clone(),
            ),
            assembler: AttachmentTransferPacketAssembler::new(
                AttachmentTransferPacketDirection::Incoming,
                quota,
            ),
            partial_timer: None,
        };
        for lane in LANES {
            native.set_buffered_amount_low_threshold(lane_index(lane), 0);
        }
        let lanes = [lane_state(budget.control()), lane_state(budget.data())];
        Arc::new_cyclic(|this| Self {
            socket_id,
            native,
            budget,
            origin: tokio::time::Instant::now(),
            closed_tx,
            this: this.clone(),
            state: Mutex::new(PortState {
                lanes,
                ingress: None,
                authenticated: false,
                closed: false,
                hello_timer: None,
                drain_timer: None,
                close_when_drained: None,
            }),
        })
    }

    /// Binds the one ingress; refused once closed or already bound.
    pub fn attach_ingress(&self, ingress: Arc<dyn AttachmentPeerIngress>) -> bool {
        let mut state = self.lock();
        if state.closed || state.ingress.is_some() {
            return false;
        }
        state.ingress = Some(ingress);
        true
    }

    /// The ingress, once, for the connection to tell it the port closed.
    pub fn take_ingress(&self) -> Option<Arc<dyn AttachmentPeerIngress>> {
        self.lock().ingress.take()
    }

    pub fn channel_opened(&self, lane: PeerChannelLane) {
        let mut state = self.lock();
        if state.closed {
            return;
        }
        if lane == PeerChannelLane::Control && !state.authenticated && state.hello_timer.is_none() {
            state.hello_timer = Some(self.arm_hello_deadline());
        }
        self.flush(&mut state);
    }

    pub fn buffered_amount_low(&self) {
        let mut state = self.lock();
        self.flush(&mut state);
    }

    pub fn channel_failed(&self, reason: &str) {
        let mut state = self.lock();
        self.close_locked(&mut state, reason);
    }

    /// One native message: bounded and reassembled under the lock, handed to
    /// the ingress after it is released.
    pub fn channel_message(&self, lane: PeerChannelLane, binary: bool, data: &[u8]) {
        let delivery = {
            let mut state = self.lock();
            self.reassemble(&mut state, lane, binary, data)
        };
        if let Some((ingress, frame)) = delivery {
            ingress.on_message(lane, frame);
        }
    }

    fn reassemble(
        &self,
        state: &mut PortState,
        lane: PeerChannelLane,
        binary: bool,
        data: &[u8],
    ) -> Option<(Arc<dyn AttachmentPeerIngress>, Vec<u8>)> {
        let ingress = match &state.ingress {
            Some(ingress) if !state.closed && binary => Arc::clone(ingress),
            _ => {
                self.close_locked(state, "unexpected_client_data");
                return None;
            }
        };
        if !state.authenticated && lane != PeerChannelLane::Control {
            self.close_locked(state, "unauthenticated_client_data");
            return None;
        }
        if data.len() > PACKET_MAX_BYTES {
            self.close_locked(state, "packet_too_large");
            return None;
        }
        if !state.authenticated {
            match parse_attachment_transfer_packet(data) {
                Ok(packet) if packet.header.total_bytes as usize > PACKET_MAX_BYTES => {
                    self.close_locked(state, "unauthenticated_frame_too_large");
                    return None;
                }
                Ok(_) => {}
                Err(_) => {
                    self.close_locked(state, "packet_rejected");
                    return None;
                }
            }
        }
        let now_ms = self.now_ms();
        let pushed = state.lanes[lane_index(lane)].assembler.push(data, now_ms);
        let Ok(complete) = pushed else {
            self.close_locked(state, "packet_rejected");
            return None;
        };
        self.arm_partial_deadline(state, lane);
        complete.map(|frame| (ingress, frame))
    }

    fn arm_partial_deadline(&self, state: &mut PortState, lane: PeerChannelLane) {
        let lane_state = &mut state.lanes[lane_index(lane)];
        if let Some(timer) = lane_state.partial_timer.take() {
            timer.abort();
        }
        if !lane_state.assembler.has_partial_message() {
            return;
        }
        let this = self.this.clone();
        lane_state.partial_timer = Some(
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(PACKET_STALL_MS)).await;
                let Some(port) = this.upgrade() else {
                    return;
                };
                let mut state = port.lock();
                let lane_state = &mut state.lanes[lane_index(lane)];
                lane_state.partial_timer = None;
                let now_ms = port.now_ms();
                if lane_state.assembler.expire(now_ms) {
                    port.close_locked(&mut state, "packet_stalled");
                }
            })
            .abort_handle(),
        );
    }

    fn arm_hello_deadline(&self) -> AbortHandle {
        let this = self.this.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(HELLO_DEADLINE_MS)).await;
            let Some(port) = this.upgrade() else {
                return;
            };
            let mut state = port.lock();
            state.hello_timer = None;
            if !state.authenticated {
                port.close_locked(&mut state, "hello_timeout");
            }
        })
        .abort_handle()
    }

    pub(super) fn close_locked(&self, state: &mut PortState, reason: &str) {
        if state.closed {
            return;
        }
        state.closed = true;
        for timer in [state.hello_timer.take(), state.drain_timer.take()]
            .into_iter()
            .flatten()
        {
            timer.abort();
        }
        for lane in LANES {
            let lane_state = &mut state.lanes[lane_index(lane)];
            if let Some(timer) = lane_state.partial_timer.take() {
                timer.abort();
            }
            lane_state.queue.clear();
            lane_state.assembler.reset();
            self.native.close_channel(lane_index(lane));
        }
        self.budget.dispose();
        tracing::info!(reason, "attachment peer port closed");
        // The connection may already be gone; the close stands either way.
        let _delivered = self.closed_tx.send(reason.to_owned());
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.origin.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    pub(super) fn lock(&self) -> MutexGuard<'_, PortState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
