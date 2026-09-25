//! The bounded FIFO send path in front of the fragment assembler.
//!
//! A queue owns whole logical messages and hands out one framed packet at a
//! time, so a stalled peer cannot make the worker retain terminal output
//! without limit: admission is refused before ownership transfers, and the
//! refused buffer is still the caller's. Nothing is evicted — the oldest
//! message stays first, because dropping a prefix of an ordered lane would
//! splice the sequence.

use std::cmp::min;
use std::collections::VecDeque;
use std::fmt;

use crate::terminal_peer::packets::{
    TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES, TerminalPeerLaneByteCaps, TerminalPeerPacketError,
    TerminalPeerPacketHeader, TerminalPeerPacketLane, TerminalPeerPacketQuota,
    encode_terminal_peer_packet,
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

/// FIFO source-buffer ownership for one ordered data-channel lane.
pub struct TerminalPeerPacketQueue<Q: TerminalPeerPacketQuota> {
    pub lane: TerminalPeerPacketLane,
    quota: Q,
    messages: VecDeque<QueuedMessage>,
    pending: Option<PendingFragment>,
    pending_bytes: Vec<u8>,
    next_message_id: u32,
    retained_bytes: usize,
    closed: bool,
}

impl<Q: TerminalPeerPacketQuota> fmt::Debug for TerminalPeerPacketQueue<Q> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalPeerPacketQueue")
            .field("lane", &self.lane)
            .field("queued_bytes", &self.retained_bytes)
            .field("message_count", &self.messages.len())
            .field("closed", &self.closed)
            .finish_non_exhaustive()
    }
}

impl<Q: TerminalPeerPacketQuota> TerminalPeerPacketQueue<Q> {
    pub fn new(lane: TerminalPeerPacketLane, quota: Q) -> Self {
        Self {
            lane,
            quota,
            messages: VecDeque::new(),
            pending: None,
            pending_bytes: Vec::new(),
            next_message_id: 1,
            retained_bytes: 0,
            closed: false,
        }
    }

    /// Complete logical bytes retained by this queue, the active message
    /// included.
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
    ///
    /// `Ok(false)` is the refusal, and it happens before anything is
    /// retained: the queue is unchanged, which is the caller's signal to treat
    /// the lane as backpressured and wait for its low-water mark rather than
    /// retrying into a full queue.
    pub fn enqueue(&mut self, bytes: Vec<u8>) -> Result<bool, TerminalPeerPacketError> {
        if self.closed {
            return Err(TerminalPeerPacketError::Closed);
        }
        let length = bytes.len();
        if length < 1 || length > TerminalPeerLaneByteCaps::for_lane(self.lane) {
            return Err(TerminalPeerPacketError::MessageSize);
        }
        if self.next_message_id == 0 {
            return Err(TerminalPeerPacketError::MessageIdWrap);
        }
        if !self.quota.reserve(length) {
            return Ok(false);
        }
        // The reservation is released again if the queue itself cannot grow,
        // so a refused admission never leaves the peer charged for bytes it
        // did not retain.
        if self.messages.try_reserve(1).is_err() {
            self.quota.release(length);
            return Err(TerminalPeerPacketError::Allocation);
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

    /// Materializes at most one packet of the packet cap and caches it until
    /// it is committed. The fragment borrows the queue, so a packet can never
    /// be sent after the bytes behind it have moved on.
    pub fn next_fragment(
        &mut self,
    ) -> Result<Option<TerminalPeerPacketQueueFragment<'_, Q>>, TerminalPeerPacketError> {
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
        let (message_id, final_fragment) = match &self.pending {
            Some(pending) => (pending.message_id, pending.final_fragment),
            None => return Ok(None),
        };
        let lane = self.lane;
        Ok(Some(TerminalPeerPacketQueueFragment {
            queue: self,
            lane,
            message_id,
            final_fragment,
        }))
    }

    /// Releases every owned buffer and closes this channel generation.
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
    ) -> Result<Option<(PendingFragment, Vec<u8>)>, TerminalPeerPacketError> {
        let Some(message) = self.messages.front() else {
            return Ok(None);
        };
        let start = message.offset_bytes as usize;
        let total_bytes = message.bytes.len();
        let payload_bytes = min(TERMINAL_PEER_PACKET_MAX_PAYLOAD_BYTES, total_bytes - start);
        let header = TerminalPeerPacketHeader {
            message_id: message.message_id,
            total_bytes: total_bytes as u32,
            offset_bytes: message.offset_bytes,
        };
        let payload = &message.bytes[start..start + payload_bytes];
        let encoded = encode_terminal_peer_packet(self.lane, header, payload);
        let bytes = encoded?;
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
pub struct TerminalPeerPacketQueueFragment<'queue, Q: TerminalPeerPacketQuota> {
    queue: &'queue mut TerminalPeerPacketQueue<Q>,
    pub lane: TerminalPeerPacketLane,
    pub message_id: u32,
    pub final_fragment: bool,
}

impl<Q: TerminalPeerPacketQuota> fmt::Debug for TerminalPeerPacketQueueFragment<'_, Q> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalPeerPacketQueueFragment")
            .field("lane", &self.lane)
            .field("message_id", &self.message_id)
            .field("final_fragment", &self.final_fragment)
            .finish_non_exhaustive()
    }
}

impl<Q: TerminalPeerPacketQuota> TerminalPeerPacketQueueFragment<'_, Q> {
    pub fn bytes(&self) -> &[u8] {
        &self.queue.pending_bytes
    }

    /// Advances the queue past this one packet. Consuming the fragment is what
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
