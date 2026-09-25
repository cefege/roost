//! One channel's retained history: the output bytes and geometry records a
//! fresh worker replays to rebuild a channel it did not spawn. Owned by the
//! keeper.
//!
//! The retention is a bound on BYTES, not on records, because what a replay
//! costs is bytes. A record-count bound would let one channel's chatty program
//! evict every other channel's data.

use crate::history::{HistoryRecord, HistoryRecords};
use crate::payloads::TerminalState;

/// The default retained history per channel.
///
/// Sized so a worker that reconnects mid-session replays a screenful or two of
/// scrollback, not the whole session: a longer gap between the keeper's last
/// worker and the next one is repaired by a snapshot, not by a longer replay.
pub const DEFAULT_HISTORY_BYTES: usize = 1024 * 1024;

/// The default retained geometry records. Bounded separately because they are
/// what a late joiner needs to know WHERE to rebuild, and evicting them is what
/// forces a full snapshot instead.
pub const DEFAULT_RESIZE_RECORDS: usize = 4096;

/// A channel's history, oldest first, bounded by retained bytes.
#[derive(Debug, Clone)]
pub struct ChannelHistory {
    records: Vec<HistoryRecord>,
    retained_bytes: usize,
    max_bytes: usize,
    max_resize_records: usize,
    /// The highest sequence written, which is what `GetHistoryResp` reports as
    /// the head. A client uses it to know whether it missed anything.
    head_seq: u64,
}

impl ChannelHistory {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_HISTORY_BYTES, DEFAULT_RESIZE_RECORDS)
    }

    pub fn with_limits(max_bytes: usize, max_resize_records: usize) -> Self {
        Self {
            records: Vec::new(),
            retained_bytes: 0,
            max_bytes,
            max_resize_records,
            head_seq: 0,
        }
    }

    /// The sequence the keeper has emitted up to, whether or not it is still
    /// retained. A client comparing this against what it has is how a gap is
    /// detected, so it must survive eviction.
    pub fn head_seq(&self) -> u64 {
        self.head_seq
    }

    /// How many bytes are retained, for `roost doctor` and for the eviction
    /// tests to assert against.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Record PTY output at `seq`.
    ///
    /// A sequence at or below the head is dropped rather than stored: history
    /// is a log, and a duplicate would make a replay ambiguous about which
    /// copy of a byte the client already had.
    pub fn record_output(&mut self, seq: u64, bytes: &[u8]) {
        if seq <= self.head_seq || bytes.is_empty() {
            return;
        }
        self.head_seq = seq;
        self.push(HistoryRecord::Output {
            seq,
            bytes: bytes.to_vec(),
        });
    }

    /// Record a geometry change at `seq`. Older sequences are dropped: geometry
    /// only makes sense in order, and a stale resize would move a rejoining
    /// client's terminal to where it used to be.
    pub fn record_resize(&mut self, seq: u64, state: TerminalState) {
        if seq <= self.head_seq {
            return;
        }
        self.head_seq = seq;
        self.push(HistoryRecord::Resize {
            seq,
            cols: state.cols,
            rows: state.rows,
        });
    }

    /// Append and evict from the front until the bounds hold.
    fn push(&mut self, record: HistoryRecord) {
        let size = record_size(&record);
        self.records.push(record);
        self.retained_bytes += size;
        self.evict();
    }

    /// Drop the oldest records until both bounds hold.
    ///
    /// Geometry records are evicted FIRST, deliberately: a stale geometry
    /// record is worth less than stale output, because a client that lost the
    /// geometry records can still replay the bytes and ask for a snapshot,
    /// whereas one that replayed bytes against the wrong geometry paints a
    /// screen that was never on that terminal.
    fn evict(&mut self) {
        while self.retained_bytes > self.max_bytes && self.records.len() > 1 {
            self.drop_oldest();
        }
        let mut resize_records = self.resize_record_count();
        while resize_records > self.max_resize_records && self.records.len() > 1 {
            // Find the oldest geometry record and drop it, not merely the
            // oldest record: the byte bound wants the oldest bytes, and this
            // bound wants the oldest geometry.
            match self
                .records
                .iter()
                .position(|r| matches!(r, HistoryRecord::Resize { .. }))
            {
                Some(index) => {
                    self.records.remove(index);
                    self.retained_bytes = self.records.iter().map(record_size).sum();
                    resize_records -= 1;
                }
                None => break,
            }
        }
    }

    fn drop_oldest(&mut self) {
        let removed = self.records.remove(0);
        self.retained_bytes -= record_size(&removed);
    }

    fn resize_record_count(&self) -> usize {
        self.records
            .iter()
            .filter(|r| matches!(r, HistoryRecord::Resize { .. }))
            .count()
    }

    /// Everything still retained, oldest first — the order a replay must use.
    pub fn records(&self) -> HistoryRecords {
        HistoryRecords {
            records: self.records.clone(),
        }
    }

    /// What a late joiner needs to know before it replays: the head sequence
    /// and whether the retained history actually starts where the client
    /// thinks it does.
    pub fn drain_for(&self, after_seq: u64) -> HistoryRecords {
        HistoryRecords {
            records: self
                .records
                .iter()
                .filter(|record| record_sequence(record) > after_seq)
                .cloned()
                .collect(),
        }
    }
}

impl Default for ChannelHistory {
    fn default() -> Self {
        Self::new()
    }
}

impl HistoryRecord {
    /// The sequence this record sits at.
    pub fn sequence(&self) -> u64 {
        record_sequence(self)
    }
}

fn record_sequence(record: &HistoryRecord) -> u64 {
    match record {
        HistoryRecord::Output { seq, .. } | HistoryRecord::Resize { seq, .. } => *seq,
    }
}

/// What one record costs in retained bytes: its sequence and length, not its
/// contents alone, so a record of empty output is not free.
fn record_size(record: &HistoryRecord) -> usize {
    const OVERHEAD: usize = 16;
    match record {
        HistoryRecord::Output { bytes, .. } => OVERHEAD + bytes.len(),
        HistoryRecord::Resize { .. } => OVERHEAD + 4,
    }
}
