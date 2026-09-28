//! Bounded record pools for one armed recording: the byte budgets, the
//! oldest-first eviction, and the omission ledger that names every evicted
//! range. Ports `apps/worker/src/diag/terminal-capture-pools.ts`; called by
//! `super::recorder_state`, `super::emission` and `super::recorder`. Every
//! ceiling comes from `TERMINAL_CAPTURE_LIMITS`.

use std::collections::VecDeque;

use roost_protocol::cell::{CellGridFrame, CellRow};
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::bundle::{
    TerminalCaptureDropCounters, TerminalCaptureOffsetRange, TerminalCaptureOmission,
    TerminalCaptureOmissionKind as Kind, TerminalCoverageReason as Reason,
    TerminalWorkerResizeRecord,
};

/// Flat-rate JSON cost of one record's scalar envelope: retention is decided on
/// the emission path, where stringifying a frame to measure it would cost more
/// than retaining it.
const FRAME_BASE_BYTES: u64 = 384;
const ROW_BASE_BYTES: u64 = 24;
const SPAN_BASE_BYTES: u64 = 56;
const RAW_RECORD_BASE_BYTES: u64 = 160;
pub const SEGMENT_RECORD_BYTES: u64 = 288;
pub const RESIZE_RECORD_BYTES: u64 = 320;

/// Which budget a pool draws from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BytePool {
    Raw,
    Cell,
    Metadata,
}

/// One pool's name, omission vocabulary and budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoolSpec {
    pub name: &'static str,
    pub kind: Kind,
    pub reason: Reason,
    pub budget: BytePool,
}

pub const RAW_POOL: PoolSpec = PoolSpec {
    name: "worker.raw",
    kind: Kind::Raw,
    reason: Reason::RawPrefixEvicted,
    budget: BytePool::Raw,
};
pub const EMISSION_POOL: PoolSpec = PoolSpec {
    name: "worker.emissions",
    kind: Kind::Records,
    reason: Reason::SegmentEvicted,
    budget: BytePool::Cell,
};
pub const CORE_SAMPLE_POOL: PoolSpec = PoolSpec {
    name: "worker.core_samples",
    kind: Kind::Sample,
    reason: Reason::SegmentEvicted,
    budget: BytePool::Cell,
};
pub const RESIZE_POOL: PoolSpec = PoolSpec {
    name: "worker.resizes",
    kind: Kind::Records,
    reason: Reason::MissingResizeBoundary,
    budget: BytePool::Metadata,
};
pub const SEGMENT_POOL: PoolSpec = PoolSpec {
    name: "worker.segments",
    kind: Kind::Records,
    reason: Reason::SegmentEvicted,
    budget: BytePool::Metadata,
};

/// One retained record, its budgeted cost, and the absolute offsets it covers
/// so eviction names exactly what the bundle no longer contains.
#[derive(Debug, Clone)]
pub struct Retained<T> {
    pub record: T,
    pub bytes: u64,
    pub range: Option<(u64, u64)>,
}

/// One coalesced omission, keyed by (kind, name, reason).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutableOmission {
    pub spec: PoolSpec,
    pub dropped_count: u64,
    pub dropped_bytes: u64,
    pub range: Option<(u64, u64)>,
}

/// Everything a pool eviction moves: the three budgets, the drop counters, the
/// omission ledger and whether the raw chain still reaches its first byte.
#[derive(Debug, Clone, Default)]
pub struct RetentionLedger {
    pub raw_bytes: u64,
    pub cell_bytes: u64,
    pub metadata_bytes: u64,
    pub dropped: TerminalCaptureDropCounters,
    pub omissions: Vec<MutableOmission>,
    pub raw_prefix_complete: bool,
}

impl RetentionLedger {
    pub fn retained_bytes(&self) -> u64 {
        self.raw_bytes + self.cell_bytes + self.metadata_bytes
    }

    fn used(&self, pool: BytePool) -> u64 {
        match pool {
            BytePool::Raw => self.raw_bytes,
            BytePool::Cell => self.cell_bytes,
            BytePool::Metadata => self.metadata_bytes,
        }
    }

    fn used_mut(&mut self, pool: BytePool) -> &mut u64 {
        match pool {
            BytePool::Raw => &mut self.raw_bytes,
            BytePool::Cell => &mut self.cell_bytes,
            BytePool::Metadata => &mut self.metadata_bytes,
        }
    }

    /// Coalesced by (kind, name, reason) so the ledger stays bounded by the few
    /// things that can be dropped, not by how often they were.
    pub fn note_omission(&mut self, spec: PoolSpec, bytes: u64, range: Option<(u64, u64)>) {
        let found = self.omissions.iter_mut().find(|omission| {
            (omission.spec.kind, omission.spec.name, omission.spec.reason)
                == (spec.kind, spec.name, spec.reason)
        });
        let Some(existing) = found else {
            self.omissions.push(MutableOmission {
                spec,
                dropped_count: 1,
                dropped_bytes: bytes,
                range,
            });
            return;
        };
        existing.dropped_count += 1;
        existing.dropped_bytes += bytes;
        if let Some((start, end)) = range {
            existing.range = Some(match existing.range {
                None => (start, end),
                Some((held_start, held_end)) => (held_start.min(start), held_end.max(end)),
            });
        }
    }

    /// The ledger as the bundle names it.
    pub fn omissions(&self) -> Vec<TerminalCaptureOmission> {
        self.omissions
            .iter()
            .map(|omission| TerminalCaptureOmission {
                kind: omission.spec.kind,
                name: omission.spec.name.to_owned(),
                reason: omission.spec.reason,
                dropped_count: omission.dropped_count,
                dropped_bytes: omission.dropped_bytes,
                range: omission
                    .range
                    .map(|(start, end)| TerminalCaptureOffsetRange {
                        start: start.to_string(),
                        end: end.to_string(),
                    }),
            })
            .collect()
    }
}

fn budget_cap(pool: BytePool) -> u64 {
    let limits = TERMINAL_CAPTURE_LIMITS;
    match pool {
        BytePool::Raw => limits.raw_bytes as u64,
        BytePool::Cell => limits.cell_bytes as u64,
        BytePool::Metadata => limits.metadata_bytes as u64,
    }
}

/// Approximate JSON cost of one frame: O(spans) rather than O(stringify), so
/// the emission path can pay it inline and still enforce a megabyte budget.
pub fn approximate_cell_frame_bytes(frame: &CellGridFrame) -> u64 {
    FRAME_BASE_BYTES
        + approximate_cell_rows_bytes(&frame.viewport_rows)
        + approximate_cell_rows_bytes(&frame.scrollback_rows)
        + approximate_cell_rows_bytes(&frame.scrollback_append)
}

/// Text is measured in UTF-16 code units, as v2's `string.length` measured it.
pub fn approximate_cell_rows_bytes(rows: &[CellRow]) -> u64 {
    let units = |text: Option<&String>| text.map_or(0, |text| text.encode_utf16().count() as u64);
    rows.iter()
        .map(|row| {
            ROW_BASE_BYTES
                + row
                    .spans
                    .iter()
                    .map(|span| {
                        SPAN_BASE_BYTES
                            + units(Some(&span.text))
                            + units(span.link_uri.as_ref())
                            + units(span.link_key.as_ref())
                    })
                    .sum::<u64>()
        })
        .sum()
}

/// base64 inflates by 4/3 and the bundle carries the encoded form.
pub fn raw_record_bytes(byte_length: usize) -> u64 {
    RAW_RECORD_BASE_BYTES + (byte_length as u64).div_ceil(3) * 4
}

/// Retain one record, then evict oldest-first until every entry and byte bound
/// holds again. `false` when the record ALONE exceeds its budget: the caller
/// marks the segment unavailable rather than retaining an unbounded object.
pub fn retain_worker_record<T>(
    ledger: &mut RetentionLedger,
    pool: &mut VecDeque<Retained<T>>,
    spec: PoolSpec,
    record: T,
    bytes: u64,
    range: Option<(u64, u64)>,
) -> bool {
    if bytes > budget_cap(spec.budget) {
        return false;
    }
    pool.push_back(Retained {
        record,
        bytes,
        range,
    });
    *ledger.used_mut(spec.budget) += bytes;
    while pool.len() > TERMINAL_CAPTURE_LIMITS.layer_entries
        || ledger.used(spec.budget) > budget_cap(spec.budget)
        || ledger.retained_bytes() > TERMINAL_CAPTURE_LIMITS.layer_bytes as u64
    {
        let Some(victim) = pool.pop_front() else {
            break;
        };
        *ledger.used_mut(spec.budget) -= victim.bytes;
        ledger.dropped.records += 1;
        ledger.dropped.bytes += victim.bytes;
        if spec.budget == BytePool::Raw {
            ledger.dropped.raw_bytes += victim.bytes;
            // The retained window no longer reaches the recording's first byte,
            // so exact parser replay from this core's init is off the table.
            ledger.raw_prefix_complete = false;
        }
        if spec == CORE_SAMPLE_POOL {
            ledger.dropped.samples += 1;
        }
        ledger.note_omission(spec, victim.bytes, victim.range);
    }
    true
}

/// Retain one resize record under the metadata budget.
pub fn retain_resize_record(
    ledger: &mut RetentionLedger,
    resizes: &mut VecDeque<TerminalWorkerResizeRecord>,
    record: TerminalWorkerResizeRecord,
) {
    resizes.push_back(record);
    ledger.metadata_bytes += RESIZE_RECORD_BYTES;
    while resizes.len() > TERMINAL_CAPTURE_LIMITS.layer_entries
        || ledger.metadata_bytes > TERMINAL_CAPTURE_LIMITS.metadata_bytes as u64
    {
        if resizes.pop_front().is_none() {
            break;
        }
        ledger.metadata_bytes -= RESIZE_RECORD_BYTES;
        ledger.dropped.records += 1;
        ledger.dropped.bytes += RESIZE_RECORD_BYTES;
        ledger.note_omission(RESIZE_POOL, RESIZE_RECORD_BYTES, None);
    }
}
