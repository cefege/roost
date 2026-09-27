//! FIFO source-buffer ownership for one ordered attachment lane, outbound. A
//! framed packet stays owned by the queue until the transport accepts it, and
//! admission is refused before ownership transfers. Ported from
//! `attachment-transfer-packet-queue.ts`. Depends on the framing and the budget
//! in the parent module.

use std::collections::VecDeque;

use super::{
    ATTACHMENT_LOGICAL_FRAME_MAX_BYTES, ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES, AttachmentPacketError,
    AttachmentPacketHeader, AttachmentPacketQuota, PeerLane, encode_attachment_packet,
};

struct QueuedMessage {
    message_id: u32,
    bytes: Vec<u8>,
    offset_bytes: u32,
}

struct PendingFragment {
    message_id: u32,
    payload_bytes: usize,
    final_fragment: bool,
}

/// One lane's outbound FIFO.
#[derive(Debug, PartialEq, Eq)]
pub struct AttachmentPacketQueue {
    lane: PeerLane,
    quota: AttachmentPacketQuota,
    messages: VecDeque<QueuedMessage>,
    pending: Option<PendingFragment>,
    pending_bytes: Vec<u8>,
    next_message_id: u32,
    retained_bytes: usize,
    closed: bool,
}

impl AttachmentPacketQueue {
    /// An empty queue on `lane`, bounded by that lane's own budget.
    #[must_use]
    pub fn new(lane: PeerLane) -> Self {
        Self {
            lane,
            quota: AttachmentPacketQuota::for_lane(lane),
            messages: VecDeque::new(),
            pending: None,
            pending_bytes: Vec::new(),
            next_message_id: 1,
            retained_bytes: 0,
            closed: false,
        }
    }

    /// Complete logical bytes this queue is holding, the active message
    /// included.
    #[must_use]
    pub fn queued_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// How many whole frames are waiting.
    #[must_use]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Whether this lane's generation is closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Take ownership of one whole frame, without copying or pre-fragmenting.
    ///
    /// `Ok(false)` is the refusal, and it happens before anything is retained:
    /// the queue is unchanged, which is the caller's signal to wait for the
    /// channel's low-water mark rather than retry into a full queue.
    pub fn enqueue(&mut self, bytes: Vec<u8>) -> Result<bool, AttachmentPacketError> {
        if self.closed {
            return Err(AttachmentPacketError::Closed);
        }
        let length = bytes.len();
        if length < 1 || length > ATTACHMENT_LOGICAL_FRAME_MAX_BYTES {
            return Err(AttachmentPacketError::MessageSize);
        }
        if self.next_message_id == 0 {
            return Err(AttachmentPacketError::MessageIdWrap);
        }
        if !self.quota.reserve(length) {
            return Ok(false);
        }
        // The reservation is handed back if the queue itself cannot grow, so a
        // refused admission never leaves the lane charged for bytes it did not
        // retain.
        if self.messages.try_reserve(1).is_err() {
            self.quota.release(length);
            return Err(AttachmentPacketError::Allocation);
        }
        let message_id = self.next_message_id;
        self.messages.push_back(QueuedMessage {
            message_id,
            bytes,
            offset_bytes: 0,
        });
        self.retained_bytes += length;
        self.next_message_id = if message_id == u32::MAX {
            0
        } else {
            message_id + 1
        };
        Ok(true)
    }

    /// Materializes at most one packet of the packet cap and caches it until it
    /// is committed. The fragment borrows the queue, so a packet can never be
    /// sent after the bytes behind it have moved on.
    pub fn next_fragment(
        &mut self,
    ) -> Result<Option<AttachmentPacketQueueFragment<'_>>, AttachmentPacketError> {
        if self.pending.is_none() {
            match self.encode_next_fragment() {
                Ok(Some((pending, bytes))) => {
                    self.pending = Some(pending);
                    self.pending_bytes = bytes;
                }
                Ok(None) => {}
                Err(error) => {
                    self.clear();
                    return Err(error);
                }
            }
        }
        let lane = self.lane;
        let (message_id, final_fragment) = match &self.pending {
            Some(pending) => (pending.message_id, pending.final_fragment),
            None => return Ok(None),
        };
        Ok(Some(AttachmentPacketQueueFragment {
            queue: self,
            lane,
            message_id,
            final_fragment,
        }))
    }

    /// Releases every owned buffer and closes this lane's generation.
    pub fn clear(&mut self) {
        let drained: Vec<QueuedMessage> = self.messages.drain(..).collect();
        self.closed = true;
        self.pending = None;
        self.pending_bytes = Vec::new();
        self.retained_bytes = 0;
        for message in drained {
            self.quota.release(message.bytes.len());
        }
    }

    /// Releases this generation and opens a fresh message-id sequence.
    pub fn reset(&mut self) {
        self.clear();
        self.next_message_id = 1;
        self.closed = false;
    }

    fn encode_next_fragment(
        &self,
    ) -> Result<Option<(PendingFragment, Vec<u8>)>, AttachmentPacketError> {
        let Some(message) = self.messages.front() else {
            return Ok(None);
        };
        let start = message.offset_bytes as usize;
        let total_bytes = message.bytes.len();
        let payload_bytes = ATTACHMENT_PACKET_MAX_PAYLOAD_BYTES.min(total_bytes - start);
        let header = AttachmentPacketHeader {
            message_id: message.message_id,
            total_bytes: total_bytes as u32,
            offset_bytes: message.offset_bytes,
        };
        let payload = &message.bytes[start..start + payload_bytes];
        let bytes = encode_attachment_packet(header, payload)?;
        Ok(Some((
            PendingFragment {
                message_id: header.message_id,
                payload_bytes,
                final_fragment: start + payload_bytes == total_bytes,
            },
            bytes,
        )))
    }
}

/// A materialized packet, owned by its queue until `commit` confirms the send
/// was accepted. Dropping it without committing leaves the message in place,
/// and the next `next_fragment` produces the same bytes.
#[derive(Debug)]
pub struct AttachmentPacketQueueFragment<'queue> {
    queue: &'queue mut AttachmentPacketQueue,
    /// The lane this packet belongs to.
    pub lane: PeerLane,
    pub message_id: u32,
    pub final_fragment: bool,
}

impl AttachmentPacketQueueFragment<'_> {
    /// The bytes to hand the channel.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.queue.pending_bytes
    }

    /// Advance the queue past this one packet. Consuming the fragment is what
    /// makes the commit happen once: there is no second handle to commit with.
    pub fn commit(self) {
        let queue = self.queue;
        let Some(pending) = queue.pending.take() else {
            return;
        };
        let Some(message) = queue.messages.front_mut() else {
            return;
        };
        message.offset_bytes += pending.payload_bytes as u32;
        if message.offset_bytes as usize != message.bytes.len() {
            return;
        }
        let Some(completed) = queue.messages.pop_front() else {
            return;
        };
        queue.retained_bytes -= completed.bytes.len();
        queue.quota.release(completed.bytes.len());
    }
}
