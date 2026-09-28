//! One announced channel's held state: its buffered frames in arrival order,
//! what each lane has lost, the cell run a delta must continue, and the
//! admission rules a frame must pass to be held.
//!
//! Owned by `worker_link::announced_barrier`, which decides when a channel
//! drains or drops. Ports the per-channel half of
//! `apps/coord/src/events/announced-channel-barrier.ts` (`AnnouncedChannel`,
//! `BufferedFrame`, and `enqueue`'s refusal order, `:139-237`).

use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, TerminalMetadata};
use tokio::time::Instant;

use crate::worker_link::announced_retention::{
    RetainedMetadata, is_compact_terminal_metadata, merge_terminal_metadata,
};
use crate::worker_link::announced_types::{
    ANNOUNCED_CHANNEL_MAX_BYTES, ANNOUNCED_CHANNEL_MAX_FRAMES, ChannelPhase, DropReason,
};
use crate::worker_link::retained_budget::{RetainOutcome, RetainedWorkBudget};

/// The lane a held frame is on, with a cell's sequence claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Lane {
    Cell { full: bool, seq: u64 },
    Metadata,
    Binary { bytes: u64 },
}

/// What one channel lost, per lane.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LaneCounts {
    pub(super) cell_frames: usize,
    pub(super) metadata_frames: usize,
    pub(super) binary_frames: usize,
    pub(super) binary_bytes: u64,
}

impl LaneCounts {
    fn of(lane: Lane) -> Self {
        let mut counts = Self::default();
        counts.add(lane);
        counts
    }

    fn add(&mut self, lane: Lane) {
        match lane {
            Lane::Cell { .. } => self.cell_frames += 1,
            Lane::Metadata => self.metadata_frames += 1,
            Lane::Binary { bytes } => {
                self.binary_frames += 1;
                self.binary_bytes += bytes;
            }
        }
    }
}

/// One held frame and the budget charge it carries while `retained`.
#[derive(Debug)]
pub(super) struct BufferedFrame {
    pub(super) frame: CoordWorkerUpstream,
    pub(super) encoded_bytes: u64,
    pub(super) lane: Lane,
    pub(super) retained: bool,
}

/// One announced channel's held state.
#[derive(Debug)]
pub(super) struct Channel {
    pub(super) session_id: String,
    pub(super) phase: ChannelPhase,
    pub(super) buffered: Vec<BufferedFrame>,
    pub(super) bytes: u64,
    pub(super) counts: LaneCounts,
    pub(super) saw_cell_frame: bool,
    pub(super) last_cell_seq: u64,
    /// The latest metadata entry, which a later fact coalesces into.
    pub(super) metadata: Option<usize>,
    pub(super) deadline: Instant,
}

impl Channel {
    pub(super) fn new(session_id: &str, deadline: Instant) -> Self {
        Self {
            session_id: session_id.to_owned(),
            phase: ChannelPhase::Pending,
            buffered: Vec::new(),
            bytes: 0,
            counts: LaneCounts::default(),
            saw_cell_frame: false,
            last_cell_seq: 0,
            metadata: None,
            deadline,
        }
    }

    pub(super) fn append_retained_metadata(&mut self, fact: RetainedMetadata) {
        self.bytes += fact.encoded_bytes;
        self.counts.add(Lane::Metadata);
        self.metadata = Some(self.buffered.len());
        self.buffered.push(BufferedFrame {
            frame: CoordWorkerUpstream::TerminalMetadata(fact.metadata),
            encoded_bytes: fact.encoded_bytes,
            lane: Lane::Metadata,
            retained: true,
        });
    }

    fn latest_metadata(&self) -> Option<(usize, &TerminalMetadata)> {
        let index = self.metadata?;
        match &self.buffered.get(index)?.frame {
            CoordWorkerUpstream::TerminalMetadata(metadata) => Some((index, metadata)),
            _ => None,
        }
    }

    /// Hold one frame, or name the drop it earns and what it cost
    /// (`announced-channel-barrier.ts:139-237`, in its order).
    pub(super) fn admit(
        &mut self,
        frame: CoordWorkerUpstream,
        encoded_bytes: u64,
        budget: &mut RetainedWorkBudget,
    ) -> Result<(), (DropReason, LaneCounts)> {
        let Some(lane) = frame_lane(&frame) else {
            return Err((DropReason::OutOfOrder, LaneCounts::default()));
        };
        let rejected = LaneCounts::of(lane);
        let overflow = Err((DropReason::Overflow, rejected));
        let incoming = match &frame {
            CoordWorkerUpstream::TerminalMetadata(metadata) => Some(metadata),
            _ => None,
        };
        let is_metadata = incoming.is_some();
        if encoded_bytes == 0 || (is_metadata && !is_compact_terminal_metadata(encoded_bytes)) {
            return overflow;
        }
        let replacement = incoming.and_then(|incoming| {
            let (index, previous) = self.latest_metadata()?;
            Some((index, merge_terminal_metadata(previous, incoming)))
        });
        let (replacement, retained_frame, retained_bytes) = match replacement {
            Some((_, None)) => return overflow,
            Some((index, Some(merged))) => (
                Some(index),
                CoordWorkerUpstream::TerminalMetadata(merged.metadata),
                merged.encoded_bytes,
            ),
            None => (None, frame, encoded_bytes),
        };
        let previous_bytes = replacement.map_or(0, |index| self.buffered[index].encoded_bytes);
        let held_frames = self.buffered.len() - usize::from(replacement.is_some());
        let held_bytes = self.bytes - previous_bytes;
        if (is_metadata && !is_compact_terminal_metadata(retained_bytes))
            || held_frames >= ANNOUNCED_CHANNEL_MAX_FRAMES
            || held_bytes + retained_bytes > ANNOUNCED_CHANNEL_MAX_BYTES
        {
            return overflow;
        }
        if let Lane::Cell { full: false, seq } = lane
            && (!self.saw_cell_frame || self.last_cell_seq.checked_add(1) != Some(seq))
        {
            return Err((DropReason::OutOfOrder, rejected));
        }
        if let Some(index) = replacement {
            release_frame(&mut self.buffered[index], budget);
        }
        if budget.retain(retained_bytes) != RetainOutcome::Retained {
            if replacement.is_some() {
                self.metadata = None;
            }
            return overflow;
        }
        if let Some(index) = replacement {
            let entry = &mut self.buffered[index];
            entry.frame = retained_frame;
            entry.encoded_bytes = retained_bytes;
            entry.retained = true;
            self.bytes = held_bytes + retained_bytes;
            return Ok(());
        }
        if let Lane::Cell { seq, .. } = lane {
            self.saw_cell_frame = true;
            self.last_cell_seq = seq;
        }
        if lane == Lane::Metadata {
            self.metadata = Some(self.buffered.len());
        }
        self.counts.add(lane);
        self.bytes += retained_bytes;
        self.buffered.push(BufferedFrame {
            frame: retained_frame,
            encoded_bytes: retained_bytes,
            lane,
            retained: true,
        });
        Ok(())
    }
}

/// The lane a frame holds on, `None` for a frame no lane holds.
fn frame_lane(frame: &CoordWorkerUpstream) -> Option<Lane> {
    match frame {
        CoordWorkerUpstream::CellGrid(grid) => grid.frame.as_option().map(|cell| Lane::Cell {
            full: cell.full,
            seq: cell.seq,
        }),
        CoordWorkerUpstream::CellGridChunk(chunk) => chunk
            .chunk
            .as_option()
            .and_then(|chunk| chunk.part.as_option())
            .map(|part| Lane::Cell {
                full: part.full,
                seq: part.seq,
            }),
        CoordWorkerUpstream::Binary(binary) => Some(Lane::Binary {
            bytes: u64::try_from(binary.data.len()).unwrap_or(u64::MAX),
        }),
        CoordWorkerUpstream::TerminalMetadata(_) => Some(Lane::Metadata),
        _ => None,
    }
}

/// Take one frame off a draining channel's counters.
pub(super) fn remove_lane(counts: &mut LaneCounts, lane: Lane) {
    match lane {
        Lane::Cell { .. } => counts.cell_frames -= 1,
        Lane::Metadata => counts.metadata_frames -= 1,
        Lane::Binary { bytes } => {
            counts.binary_frames -= 1;
            counts.binary_bytes -= bytes;
        }
    }
}

/// Give a held frame's charge back, once.
pub(super) fn release_frame(frame: &mut BufferedFrame, budget: &mut RetainedWorkBudget) {
    if frame.retained {
        frame.retained = false;
        budget.release(frame.encoded_bytes);
    }
}
