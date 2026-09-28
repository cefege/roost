//! FIFO packet ownership for one ordered attachment channel direction. It
//! retains each source buffer until the transport accepts its last fragment
//! and releases each quota reservation once, so a buffered native send can
//! never duplicate a fragment. Ports
//! `packages/protocol/src/attachment-transfer-packet-queue.ts`; used by the
//! worker's attachment peer packet port.

use std::cmp::min;
use std::collections::VecDeque;
use std::fmt;

use super::packets::{
    AttachmentTransferPacketDirection, AttachmentTransferPacketError,
    AttachmentTransferPacketHeader, AttachmentTransferPacketQuota,
    encode_attachment_transfer_packet,
};
use super::{PACKET_LOGICAL_FRAME_MAX_BYTES, PACKET_MAX_PAYLOAD_BYTES};

struct QueuedMessage {
    message_id: u32,
    bytes: Vec<u8>,
    offset_bytes: usize,
}

struct PendingFragment {
    message_id: u32,
    payload_bytes: usize,
    final_fragment: bool,
    bytes: Vec<u8>,
}

/// Source-buffer ownership for one ordered attachment channel direction.
pub struct AttachmentTransferPacketQueue<Q: AttachmentTransferPacketQuota> {
    direction: AttachmentTransferPacketDirection,
    quota: Q,
    messages: VecDeque<QueuedMessage>,
    pending: Option<PendingFragment>,
    next_message_id: u32,
    retained_bytes: usize,
    closed: bool,
}

impl<Q: AttachmentTransferPacketQuota> fmt::Debug for AttachmentTransferPacketQueue<Q> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentTransferPacketQueue")
            .field("direction", &self.direction)
            .field("queued_bytes", &self.retained_bytes)
            .field("message_count", &self.messages.len())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl<Q: AttachmentTransferPacketQuota> AttachmentTransferPacketQueue<Q> {
    pub fn new(direction: AttachmentTransferPacketDirection, quota: Q) -> Self {
        Self {
            direction,
            quota,
            messages: VecDeque::new(),
            pending: None,
            next_message_id: 1,
            retained_bytes: 0,
            closed: false,
        }
    }

    pub fn queued_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Takes ownership of `bytes` without copying or pre-fragmenting them.
    /// `Ok(false)` is the quota refusal, decided before anything is retained.
    pub fn enqueue(&mut self, bytes: Vec<u8>) -> Result<bool, AttachmentTransferPacketError> {
        if self.closed {
            return Err(AttachmentTransferPacketError::Closed);
        }
        let length = bytes.len();
        if !(1..=PACKET_LOGICAL_FRAME_MAX_BYTES).contains(&length) {
            return Err(AttachmentTransferPacketError::MessageSize);
        }
        if self.next_message_id == 0 {
            return Err(AttachmentTransferPacketError::MessageIdWrap);
        }
        if !self.quota.reserve(self.direction, length) {
            return Ok(false);
        }
        if self.messages.try_reserve(1).is_err() {
            self.quota.release(self.direction, length);
            return Err(AttachmentTransferPacketError::Allocation);
        }
        let message_id = self.next_message_id;
        self.messages.push_back(QueuedMessage {
            message_id,
            bytes,
            offset_bytes: 0,
        });
        self.retained_bytes += length;
        self.next_message_id = message_id.checked_add(1).unwrap_or(0);
        Ok(true)
    }

    /// Materializes one outer packet and caches it until it is committed; a
    /// fragment dropped without a commit is produced again, byte for byte.
    pub fn next_fragment(
        &mut self,
    ) -> Result<Option<AttachmentTransferPacketQueueFragment<'_, Q>>, AttachmentTransferPacketError>
    {
        if self.pending.is_none() {
            match self.encode_next_fragment() {
                Ok(pending) => self.pending = pending,
                Err(error) => {
                    self.clear();
                    return Err(error);
                }
            }
        }
        let Some(pending) = &self.pending else {
            return Ok(None);
        };
        let (message_id, final_fragment) = (pending.message_id, pending.final_fragment);
        Ok(Some(AttachmentTransferPacketQueueFragment {
            queue: self,
            message_id,
            final_fragment,
        }))
    }

    /// Releases every retained source buffer and closes this generation.
    pub fn clear(&mut self) {
        let drained: Vec<QueuedMessage> = self.messages.drain(..).collect();
        self.closed = true;
        self.pending = None;
        self.retained_bytes = 0;
        for message in drained {
            self.quota.release(self.direction, message.bytes.len());
        }
    }

    /// Releases this generation and starts a fresh message-id sequence.
    pub fn reset(&mut self) {
        self.clear();
        self.next_message_id = 1;
        self.closed = false;
    }

    fn encode_next_fragment(
        &self,
    ) -> Result<Option<PendingFragment>, AttachmentTransferPacketError> {
        let Some(message) = self.messages.front() else {
            return Ok(None);
        };
        let start = message.offset_bytes;
        let total_bytes = message.bytes.len();
        let payload_bytes = min(PACKET_MAX_PAYLOAD_BYTES, total_bytes - start);
        let header = AttachmentTransferPacketHeader {
            message_id: message.message_id,
            total_bytes: u32::try_from(total_bytes)
                .map_err(|_| AttachmentTransferPacketError::MessageSize)?,
            offset_bytes: u32::try_from(start)
                .map_err(|_| AttachmentTransferPacketError::PacketHeader)?,
        };
        let bytes = encode_attachment_transfer_packet(
            header,
            &message.bytes[start..start + payload_bytes],
        )?;
        Ok(Some(PendingFragment {
            message_id: message.message_id,
            payload_bytes,
            final_fragment: start + payload_bytes == total_bytes,
            bytes,
        }))
    }
}

/// A framed packet, owned by its queue until `commit` confirms the transport
/// accepted it. Consuming the fragment is what makes the commit happen once.
pub struct AttachmentTransferPacketQueueFragment<'queue, Q: AttachmentTransferPacketQuota> {
    queue: &'queue mut AttachmentTransferPacketQueue<Q>,
    pub message_id: u32,
    pub final_fragment: bool,
}

impl<Q: AttachmentTransferPacketQuota> fmt::Debug for AttachmentTransferPacketQueueFragment<'_, Q> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AttachmentTransferPacketQueueFragment")
            .field("message_id", &self.message_id)
            .field("final_fragment", &self.final_fragment)
            .finish_non_exhaustive()
    }
}

impl<Q: AttachmentTransferPacketQuota> AttachmentTransferPacketQueueFragment<'_, Q> {
    pub fn bytes(&self) -> &[u8] {
        self.queue
            .pending
            .as_ref()
            .map_or(&[][..], |pending| pending.bytes.as_slice())
    }

    /// Advances past this one packet; the last fragment of a message releases
    /// that message's reservation.
    pub fn commit(self) {
        let queue = self.queue;
        let Some(pending) = queue.pending.take() else {
            return;
        };
        let Some(message) = queue.messages.front_mut() else {
            return;
        };
        message.offset_bytes += pending.payload_bytes;
        if message.offset_bytes != message.bytes.len() {
            return;
        }
        let Some(completed) = queue.messages.pop_front() else {
            return;
        };
        queue.retained_bytes -= completed.bytes.len();
        queue.quota.release(queue.direction, completed.bytes.len());
    }
}
