//! The old-coordinator raw-metadata compatibility lane: PTY bytes copied and
//! staged for a coordinator that has NOT negotiated the semantic metadata
//! capability, and dropped the moment it has. `runtime` drains it into the
//! link's outbox. Depends on `roost_protocol` for the channel brand and on
//! nothing that calls back into emission.
//!
//! IT IS A BOUNDED QUEUE, NOT A SECOND HISTORY. The retained PTY bytes live in
//! `SessionRecord::scrollback` and nothing here reads them. What lives here is
//! the SHORT-LIVED COPY on its way to the socket, and it exists only because
//! the coordinator's scanners need the raw stream in order: the outbox bounds
//! encoded frames, but a single chatty channel could take all of it, and a
//! per-channel cap is the only bound that says "this one session is flooding".
//!
//! The copy is not optional. A keeper read buffer is reused the moment the
//! callback returns, so a staged frame that aliased it would ship whatever the
//! next chunk wrote there.

use std::collections::{HashMap, VecDeque};

use roost_protocol::wire::brand::ChannelId;

/// The per-channel staging cap.
pub const RAW_METADATA_CHANNEL_CAP_BYTES: usize = 256 * 1024;

/// The aggregate staging cap, across every channel.
pub const RAW_METADATA_AGGREGATE_CAP_BYTES: usize = 2 * 1024 * 1024;

/// How many staged frames one drain pass hands over. A dispatch that took
/// everything in one turn starves the cell frames queued behind it.
pub const RAW_METADATA_DISPATCH_FRAME_BUDGET: usize = 32;

/// One chunk of raw PTY bytes with the offset of its end, ready to encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedRawFrame {
    pub channel_id: ChannelId,
    /// The logical byte offset of the chunk's END. The splice seam is placed
    /// per byte on this number, not inferred from a frame boundary.
    pub end_seq: u64,
    pub bytes: Vec<u8>,
}

/// The staging area for the compatibility lane, and the one place its caps live.
#[derive(Debug, Default)]
pub struct RawMetadataStage {
    negotiated: bool,
    queues: HashMap<ChannelId, VecDeque<StagedRawFrame>>,
    channel_bytes: HashMap<ChannelId, usize>,
    queued_bytes: usize,
    /// Frames refused because a cap was reached. Reported by the diagnostic
    /// snapshot, because a silently empty raw lane looks exactly like a
    /// coordinator that scans nothing.
    dropped: u64,
}

impl RawMetadataStage {
    /// Whether the coordinator negotiated the semantic metadata lane. True means
    /// this whole path is dormant, which is the state every v3 coordinator
    /// leaves it in.
    pub fn semantic_metadata_negotiated(&self) -> bool {
        self.negotiated
    }

    /// Record the coordinator's answer. Losing the capability mid-link refills
    /// the lane from the next chunk; bytes already sent are not re-sent, and a
    /// scanner that missed them is a scanner that was not there.
    pub fn set_semantic_metadata_negotiated(&mut self, negotiated: bool) {
        if self.negotiated == negotiated {
            return;
        }
        self.negotiated = negotiated;
        if negotiated {
            self.clear();
        }
        tracing::info!(
            negotiated,
            "the terminal-metadata negotiation changed the raw lane"
        );
    }

    /// Stage one chunk, unless a cap says no.
    pub fn stage(&mut self, channel_id: ChannelId, end_seq: u64, chunk: &[u8]) {
        if self.negotiated || chunk.is_empty() {
            return;
        }
        let channel_bytes = self.channel_bytes.get(&channel_id).copied().unwrap_or(0);
        if chunk.len() > RAW_METADATA_CHANNEL_CAP_BYTES
            || channel_bytes + chunk.len() > RAW_METADATA_CHANNEL_CAP_BYTES
            || self.queued_bytes + chunk.len() > RAW_METADATA_AGGREGATE_CAP_BYTES
        {
            self.dropped = self.dropped.saturating_add(1);
            tracing::warn!(
                %channel_id,
                frame_bytes = chunk.len(),
                channel_bytes,
                aggregate_bytes = self.queued_bytes,
                "a raw metadata frame was dropped: the staging caps were reached"
            );
            return;
        }
        self.queues
            .entry(channel_id)
            .or_default()
            .push_back(StagedRawFrame {
                channel_id,
                end_seq,
                bytes: chunk.to_vec(),
            });
        *self.channel_bytes.entry(channel_id).or_insert(0) += chunk.len();
        self.queued_bytes += chunk.len();
    }

    /// Take up to one dispatch budget of staged frames, oldest channel first.
    ///
    /// A channel drains to empty before the next one starts, so one busy
    /// session cannot reorder another session's bytes against each other.
    pub fn drain(&mut self) -> Vec<StagedRawFrame> {
        let mut ready: Vec<ChannelId> = self
            .queues
            .iter()
            .filter(|(_, queue)| !queue.is_empty())
            .map(|(channel_id, _)| *channel_id)
            .collect();
        ready.sort_unstable();
        let mut taken = Vec::new();
        let mut budget = RAW_METADATA_DISPATCH_FRAME_BUDGET;
        for channel_id in ready {
            if budget == 0 {
                break;
            }
            while budget > 0 {
                let Some(frame) = self.queues.get_mut(&channel_id).and_then(|q| q.pop_front())
                else {
                    break;
                };
                self.release(channel_id, frame.bytes.len());
                taken.push(frame);
                budget -= 1;
            }
        }
        taken
    }

    /// How many bytes are staged right now.
    pub fn staged_bytes(&self) -> usize {
        self.queued_bytes
    }

    /// How many frames the caps have refused since this stage was created.
    pub fn dropped_frames(&self) -> u64 {
        self.dropped
    }

    /// Drop one channel's staging, without disturbing another's.
    pub fn forget_channel(&mut self, channel_id: ChannelId) {
        self.queues.remove(&channel_id);
        if let Some(bytes) = self.channel_bytes.remove(&channel_id) {
            self.queued_bytes = self.queued_bytes.saturating_sub(bytes);
        }
    }

    fn release(&mut self, channel_id: ChannelId, bytes: usize) {
        self.queued_bytes = self.queued_bytes.saturating_sub(bytes);
        if let Some(remaining) = self.channel_bytes.get_mut(&channel_id) {
            *remaining = remaining.saturating_sub(bytes);
        }
    }

    fn clear(&mut self) {
        self.queues.clear();
        self.channel_bytes.clear();
        self.queued_bytes = 0;
    }
}
