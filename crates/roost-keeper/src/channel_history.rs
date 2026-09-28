//! One channel's retained history: the last `KEEPER_RING_CAP_BYTES` of raw
//! output, the head (every byte ever emitted), and the resize markers
//! interleaved with the retained bytes. Ports `apps/worker/src/keeper/keeper-history.ts`
//! and the channel's `outRing`/`headSeq` fields (`keeper-frame-handler.ts`);
//! `keeper::Keeper` records into it and answers `GetHistoryRecords`/`GetHistory`
//! from it. The markers are what let a resuming worker replay the exact
//! byte/geometry stream the PTY produced.

use std::collections::VecDeque;

use crate::codec::KEEPER_MAX_HISTORY_RESIZE_RECORDS;
use crate::history::{HistoryRecord, HistoryRecords};

/// The raw output a channel retains (v2 `KEEPER_RING_CAP_BYTES`).
pub const KEEPER_RING_CAP_BYTES: usize = 1024 * 1024;

/// A geometry change at a raw-output offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResizeMarker {
    /// The raw-output sequence at which the new geometry became effective.
    head_seq: u64,
    seq: u64,
    cols: u16,
    rows: u16,
}

/// A channel's history, bounded by retained bytes and by marker count.
#[derive(Debug, Clone)]
pub struct ChannelHistory {
    ring: VecDeque<u8>,
    ring_cap: usize,
    head_seq: u64,
    base_cols: u16,
    base_rows: u16,
    current_cols: u16,
    current_rows: u16,
    resizes: VecDeque<ResizeMarker>,
    max_resizes: usize,
}

impl ChannelHistory {
    /// A channel spawned at `cols`×`rows`: that geometry is the base until a
    /// marker is evicted over it.
    pub fn new(cols: u16, rows: u16) -> Self {
        Self::with_limits(
            cols,
            rows,
            KEEPER_RING_CAP_BYTES,
            KEEPER_MAX_HISTORY_RESIZE_RECORDS as usize,
        )
    }

    pub fn with_limits(cols: u16, rows: u16, ring_cap: usize, max_resizes: usize) -> Self {
        Self {
            ring: VecDeque::new(),
            ring_cap,
            head_seq: 0,
            base_cols: cols,
            base_rows: rows,
            current_cols: cols,
            current_rows: rows,
            resizes: VecDeque::new(),
            max_resizes,
        }
    }

    /// Every raw byte this channel ever emitted, retained or not.
    pub fn head_seq(&self) -> u64 {
        self.head_seq
    }

    /// How many raw bytes are retained right now.
    pub fn retained_bytes(&self) -> usize {
        self.ring.len()
    }

    /// Record one emitted chunk: the head advances by its length, the ring keeps
    /// the newest `ring_cap` bytes, and markers the ring evicted over become the
    /// base (v2 `appendToRing` + `trimEvictedResizeHistory`).
    pub fn record_output(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.head_seq += bytes.len() as u64;
        let keep = bytes.len().min(self.ring_cap);
        self.ring.extend(&bytes[bytes.len() - keep..]);
        let excess = self.ring.len().saturating_sub(self.ring_cap);
        self.ring.drain(..excess);
        self.trim_evicted();
    }

    /// Record a geometry change at the current head (v2 `appendResizeHistory`).
    /// A full marker budget discards the raw window too: retaining fewer
    /// records is truthful, while retaining bytes under an unknowable geometry
    /// is not.
    pub fn record_resize(&mut self, seq: u64, cols: u16, rows: u16) {
        if self.resizes.len() >= self.max_resizes {
            self.ring.clear();
            self.resizes.clear();
            self.base_cols = self.current_cols;
            self.base_rows = self.current_rows;
            tracing::warn!(
                head_seq = self.head_seq,
                "keeper: a channel's resize markers hit their budget; its retained window was discarded"
            );
        }
        self.resizes.push_back(ResizeMarker {
            head_seq: self.head_seq,
            seq,
            cols,
            rows,
        });
        self.current_cols = cols;
        self.current_rows = rows;
    }

    /// The retained window as ordered records, cut at each marker (v2
    /// `orderedHistory`).
    pub fn ordered(&self) -> HistoryRecords {
        let retained: Vec<u8> = self.ring.iter().copied().collect();
        let retained_tail = self.head_seq - retained.len() as u64;
        let mut records = Vec::new();
        let mut raw_seq = retained_tail;
        for marker in &self.resizes {
            if marker.head_seq < retained_tail || marker.head_seq > self.head_seq {
                continue;
            }
            if marker.head_seq > raw_seq {
                let from = (raw_seq - retained_tail) as usize;
                let to = (marker.head_seq - retained_tail) as usize;
                records.push(HistoryRecord::Output {
                    bytes: retained[from..to].to_vec(),
                });
                raw_seq = marker.head_seq;
            }
            records.push(HistoryRecord::Resize {
                seq: marker.seq,
                cols: marker.cols,
                rows: marker.rows,
            });
        }
        if raw_seq < self.head_seq {
            let from = (raw_seq - retained_tail) as usize;
            records.push(HistoryRecord::Output {
                bytes: retained[from..].to_vec(),
            });
        }
        HistoryRecords {
            head_seq: self.head_seq,
            base_cols: self.base_cols,
            base_rows: self.base_rows,
            records,
        }
    }

    /// The raw retained ring, for the legacy `GetHistoryResp` framing.
    pub fn ring_bytes(&self) -> Vec<u8> {
        self.ring.iter().copied().collect()
    }

    /// Markers at or below the retained tail describe bytes that are gone; the
    /// newest of them is the geometry the oldest retained byte was produced at.
    fn trim_evicted(&mut self) {
        let retained_tail = self.head_seq - self.ring.len() as u64;
        while let Some(evicted) = self.resizes.front().copied() {
            if evicted.head_seq > retained_tail {
                break;
            }
            self.base_cols = evicted.cols;
            self.base_rows = evicted.rows;
            self.resizes.pop_front();
        }
    }
}
