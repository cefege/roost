//! One negotiated WebRTC transport for one direct attachment upload. Called by
//! `direct` once loopback is unavailable; it owns the two static attachment
//! channels, authenticates on control and streams chunks on data. Ported from
//! `attachmentPeer.ts`. Depends on `conversation` for the frame rules,
//! `packets` for framing, and `signaling` for the SDP.

pub use super::conversation;

use roost_proto::__buffa::oneof::attachment_transfer_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::attachment_transfer_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{
    AttachmentTransferChunk, AttachmentTransferClientFrame, AttachmentTransferServerFrame,
    AttachmentTransferStatusRequest,
};

use super::conversation::{AttachmentConversation, ConversationOutcome};
use super::grant::AttachmentDirectGrant;
use super::packets::lanes::AttachmentPeerPacketLanes;
use super::packets::{ATTACHMENT_PEER_LANES, AttachmentPacketError, PeerLane};
use super::transfer::receipt::AttachmentTransferStatus;
use super::transfer::{AttachmentTransferCarrierError, InFlightChunk};

/// How long the peer has to answer the hello before it is retired.
pub const HELLO_DEADLINE_MS: u64 = 3_000;

/// How long one chunk's acknowledgement may take once its last fragment is out.
pub const ACK_DEADLINE_MS: u64 = 15_000;

/// The peer deadlines a host arms. Each is a distinct timer because they are
/// armed at distinct moments: the hello at the control channel opening, the
/// acknowledgement at the last data fragment going out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeerDeadline {
    Hello,
    Ack,
    Status,
}

/// One packet a host must put on a data channel, with the two decisions the
/// peer made about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentPeerPacket {
    pub lane: PeerLane,
    /// The framed bytes, already bounded by the packet cap.
    pub bytes: Vec<u8>,
    /// A data packet left, so upload bytes are on the wire.
    pub marks_chunk_sent: bool,
    /// The last data fragment went out with an acknowledgement outstanding and
    /// no deadline armed yet, which is the moment the ACK deadline starts.
    pub arms_ack_deadline: bool,
}

/// What one lane's queue produced for a flush pass.
///
/// A value rather than the fragment itself, because the fragment borrows the
/// queue and the caller of [`AttachmentPeerTransfer::flush`] must be able to
/// end the carrier from the failure arm.
#[derive(Debug)]
enum FlushStep {
    Packet {
        bytes: Vec<u8>,
        final_fragment: bool,
    },
    Empty,
    Failed,
}

/// The WebRTC attachment carrier.
#[derive(Debug)]
pub struct AttachmentPeerTransfer {
    conversation: AttachmentConversation,
    lanes: AttachmentPeerPacketLanes,
    grant: AttachmentDirectGrant,
    peer_id: String,
    control_open: bool,
    data_open: bool,
    authenticated: bool,
    ack_deadline_armed: bool,
    closed: bool,
}

impl AttachmentPeerTransfer {
    /// A carrier bound to one grant and one peer id, before any channel exists.
    #[must_use]
    pub fn new(grant: AttachmentDirectGrant, peer_id: &str) -> Self {
        let conversation =
            AttachmentConversation::new(&grant, &grant.request.worker_fp, "attachment peer");
        Self {
            conversation,
            lanes: AttachmentPeerPacketLanes::new(),
            grant,
            peer_id: peer_id.to_owned(),
            control_open: false,
            data_open: false,
            authenticated: false,
            ack_deadline_armed: false,
            closed: false,
        }
    }

    /// The channels this carrier needs, in creation order: fixed labels, fixed
    /// ids, both ordered, both negotiated in band. A negotiated id would let
    /// the far end choose which lane this client believes is control.
    #[must_use]
    pub fn channel_definitions(&self) -> [super::packets::AttachmentPeerLane; 2] {
        ATTACHMENT_PEER_LANES
    }

    /// The subprotocol both lanes negotiate, for a host building the channels.
    #[must_use]
    pub fn channel_protocol(&self) -> &'static str {
        super::packets::ATTACHMENT_PEER_DATA_CHANNEL_PROTOCOL
    }

    /// Whether the worker has admitted this peer, so a chunk may be sent.
    #[must_use]
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// Whether any chunk has been handed to a carrier.
    #[must_use]
    pub fn sent_chunk(&self) -> bool {
        self.conversation.sent_chunk()
    }

    /// A host channel opened. The control lane's opening is what starts the
    /// hello, and therefore the hello deadline.
    ///
    /// `Some(true)` is the moment this carrier became usable, which is when a
    /// host stops the hello deadline.
    pub fn channel_opened(
        &mut self,
        lane: PeerLane,
    ) -> Result<Option<bool>, AttachmentTransferCarrierError> {
        match lane {
            PeerLane::Control => self.control_open = true,
            PeerLane::Data => self.data_open = true,
        }
        if lane == PeerLane::Control {
            // A client frame, as v2 `attachmentPeer.ts:160-166` queues it.
            let hello = encode_client_frame(ClientFrame::Hello(Box::new(
                self.grant.hello(&self.peer_id),
            )));
            if !self.queue_frame(PeerLane::Control, &hello) {
                return Err(self.finish("attachment peer could not authenticate"));
            }
        }
        Ok(self.admit_when_channels_open())
    }

    /// Take the carrier for one chunk and frame it.
    pub fn send_chunk(
        &mut self,
        chunk: &InFlightChunk,
        data: Vec<u8>,
    ) -> Result<(), AttachmentTransferCarrierError> {
        self.conversation.begin_chunk(chunk.seq)?;
        let frame = AttachmentTransferChunk {
            upload_id: chunk.upload_id.clone(),
            seq: chunk.seq,
            data,
            last: chunk.last,
            offset: chunk.offset,
            chunk_sha256: chunk.chunk_sha256.clone(),
            ..Default::default()
        };
        if !self.queue_frame(
            PeerLane::Data,
            &encode_client_frame(ClientFrame::Chunk(Box::new(frame))),
        ) {
            let reason = "attachment peer could not queue a chunk";
            return Err(self.conversation.fail_ack(reason, false));
        }
        Ok(())
    }

    /// Take the carrier for one receipt and frame it.
    pub fn request_status(
        &mut self,
        upload_id: &str,
    ) -> Result<(), AttachmentTransferCarrierError> {
        self.conversation.begin_status(upload_id)?;
        let frame = AttachmentTransferStatusRequest {
            upload_id: upload_id.to_owned(),
            ..Default::default()
        };
        let encoded = encode_client_frame(ClientFrame::StatusRequest(Box::new(frame)));
        if !self.queue_frame(PeerLane::Control, &encoded) {
            return Err(self
                .conversation
                .fail_status("attachment peer could not request status"));
        }
        Ok(())
    }

    /// Accept one inbound packet, and settle a waiter if it completed a frame.
    pub fn packet_received(
        &mut self,
        lane: PeerLane,
        packet: &[u8],
        now_ms: u64,
    ) -> Result<ConversationOutcome, AttachmentTransferCarrierError> {
        match self.lanes.receive(lane, packet, now_ms) {
            Ok(Some(message)) => self.receive_frame(lane, &message),
            Ok(None) => Ok(ConversationOutcome::Ignored),
            Err(AttachmentPacketError::FragmentStalled) => {
                Err(self.finish("attachment peer packet stalled"))
            }
            Err(_) => Err(self.finish("attachment peer received an invalid packet")),
        }
    }

    /// A deadline a host armed elapsed.
    pub fn deadline_elapsed(
        &mut self,
        deadline: PeerDeadline,
    ) -> Result<(), AttachmentTransferCarrierError> {
        match deadline {
            PeerDeadline::Hello => Err(self.finish("attachment peer hello timed out")),
            PeerDeadline::Ack => {
                self.ack_deadline_armed = false;
                // The bytes may already be committed, so this is the one
                // acknowledgement failure a receipt can still answer for.
                Err(self
                    .conversation
                    .fail_ack("attachment peer acknowledgement timed out", true))
            }
            PeerDeadline::Status => Err(self
                .conversation
                .fail_status("attachment peer status timed out")),
        }
    }

    /// The peer connection or its ICE failed.
    pub fn transport_failed(&mut self) -> AttachmentTransferCarrierError {
        self.finish("attachment peer connection failed")
    }

    /// The packets to put on each open lane, in lane order.
    ///
    /// One packet per lane per pass, because that is all a channel accepts at a
    /// time; a host calls again while [`AttachmentPeerPacketLanes::has_queued_packets`]
    /// is true, which is what the buffered-amount-low callback drives.
    pub fn flush(
        &mut self,
        open_lanes: &[PeerLane],
    ) -> Result<Vec<AttachmentPeerPacket>, AttachmentTransferCarrierError> {
        let mut packets = Vec::new();
        for definition in ATTACHMENT_PEER_LANES {
            let lane = definition.lane;
            if !open_lanes.contains(&lane) {
                continue;
            }
            // The queue is borrowed only for this block, so the failure arm
            // below can still reach `self` to end the carrier.
            let step = {
                let queue = self.lanes.outbound_mut(lane);
                match queue.next_fragment() {
                    Ok(Some(fragment)) => {
                        let bytes = fragment.bytes().to_vec();
                        let final_fragment = fragment.final_fragment;
                        fragment.commit();
                        FlushStep::Packet {
                            bytes,
                            final_fragment,
                        }
                    }
                    Ok(None) => FlushStep::Empty,
                    Err(_) => FlushStep::Failed,
                }
            };
            let (bytes, final_fragment) = match step {
                FlushStep::Packet {
                    bytes,
                    final_fragment,
                } => (bytes, final_fragment),
                FlushStep::Empty => continue,
                FlushStep::Failed => {
                    return Err(self.finish("attachment peer data channel send failed"));
                }
            };
            let arms_ack_deadline = lane == PeerLane::Data
                && final_fragment
                && self.conversation.awaiting_ack().is_some()
                && !self.ack_deadline_armed;
            if arms_ack_deadline {
                self.ack_deadline_armed = true;
            }
            packets.push(AttachmentPeerPacket {
                lane,
                bytes,
                marks_chunk_sent: lane == PeerLane::Data,
                arms_ack_deadline,
            });
        }
        Ok(packets)
    }

    /// Whether another flush pass is owed.
    #[must_use]
    pub fn has_queued_packets(&self) -> bool {
        self.lanes.has_queued_packets()
    }

    /// End the carrier, releasing every lane and waiter.
    pub fn finish(&mut self, reason: &str) -> AttachmentTransferCarrierError {
        if self.closed {
            return self.conversation.close(reason);
        }
        self.closed = true;
        self.lanes.clear();
        self.conversation.close(reason)
    }

    fn queue_frame(&mut self, lane: PeerLane, frame: &[u8]) -> bool {
        if self.closed {
            return false;
        }
        matches!(
            self.lanes.outbound_mut(lane).enqueue(frame.to_vec()),
            Ok(true)
        )
    }

    /// `Some(true)` on the pass that authenticated this carrier, so a host
    /// knows to stop its hello deadline exactly once.
    fn admit_when_channels_open(&mut self) -> Option<bool> {
        if self.authenticated || !self.conversation.is_ready() {
            return None;
        }
        if !(self.control_open && self.data_open) {
            return None;
        }
        self.authenticated = true;
        Some(true)
    }

    fn receive_frame(
        &mut self,
        lane: PeerLane,
        message: &[u8],
    ) -> Result<ConversationOutcome, AttachmentTransferCarrierError> {
        let frame = AttachmentTransferServerFrame::decode_from_slice(message)
            .map_err(|_| self.finish("attachment peer received an invalid frame"))?;
        if !self.conversation.is_ready() {
            // Ready first, on control, and nothing else: a peer that opens with
            // an acknowledgement has not authenticated anything.
            let Some(inner) = frame.frame.as_ref() else {
                return Err(self.finish("attachment peer received an invalid frame"));
            };
            let ServerFrame::Ready(ready) = inner else {
                return Err(self.finish("attachment peer required Ready first"));
            };
            if lane != PeerLane::Control {
                return Err(self.finish("attachment peer required Ready first"));
            }
            self.conversation.admit_ready(ready)?;
            self.admit_when_channels_open();
            return Ok(ConversationOutcome::Ready);
        }
        if !self.authenticated || lane != PeerLane::Control {
            return Err(self.finish("attachment peer received an invalid frame"));
        }
        self.ack_deadline_armed = false;
        let Some(inner) = frame.frame.as_ref() else {
            return Err(self.finish("attachment peer received an invalid frame"));
        };
        match inner {
            ServerFrame::Ack(ack) => self.conversation.settle_ack(ack),
            ServerFrame::Status(status) => self.conversation.settle_status(&status_from(status)),
            ServerFrame::Closed(_) => Err(self.finish("attachment peer closed")),
            ServerFrame::Ready(_) => Err(self.finish("attachment peer received an invalid frame")),
        }
    }
}

/// The durable receipt, as the shared conversation names it.
fn status_from(status: &roost_proto::AttachmentTransferStatus) -> AttachmentTransferStatus {
    AttachmentTransferStatus {
        upload_id: status.upload_id.clone(),
        next_seq: status.next_seq,
        bytes_received: status.bytes_received,
        last_chunk_sha256: status.last_chunk_sha256.clone(),
        committed: status.committed,
        abs_path: status.abs_path.clone(),
        error: status.error.clone(),
    }
}

/// Wrap one client-frame arm for encoding.
fn encode_client_frame(frame: ClientFrame) -> Vec<u8> {
    AttachmentTransferClientFrame {
        frame: Some(frame),
        ..Default::default()
    }
    .encode_to_vec()
}
