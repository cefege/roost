//! One bounded read of a session's authoritative grid for a capture: the
//! geometry, the scrollback tail, and the absolute history rows the browser
//! named, with evicted and not-yet-written ranges reported rather than
//! omitted. Ports `readGridEvidence`/`readHistoryRanges` of `apps/worker/src/
//! diag/terminal-capture-worker-section.ts`; called by `super::worker_section`.
//! It reads the core directly and never calls a keeper history RPC.

use roost_protocol::cell::CellRow;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::bundle::{
    TerminalCaptureHistoryRange, TerminalCaptureOmission, TerminalCaptureOmissionKind,
    TerminalCaptureRangeStatus as Status, TerminalCoverageReason,
};
use roost_term::{TerminalCore, read_scrollback_range, scrollback_origin};

use super::evidence::HistoryRequest;
use crate::session::types::SessionRecord;

/// What one read of the grid found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GridEvidence {
    /// False when this session has no record here, or its core could not
    /// report its origin: geometry is then absent, never a plausible 0×0.
    pub readable: bool,
    pub cols: u32,
    pub rows: u32,
    pub origin: u64,
    pub total: u64,
    pub tail: Vec<CellRow>,
    pub history_rows: Vec<CellRow>,
    pub ranges: Vec<TerminalCaptureHistoryRange>,
}

/// One bounded read of the live grid, OUTSIDE any per-emission loop. A core
/// that cannot report its origin is recorded as missing evidence.
pub fn read_grid_evidence(
    record: Option<&SessionRecord>,
    requested: &[HistoryRequest],
    omissions: &mut Vec<TerminalCaptureOmission>,
) -> GridEvidence {
    let Some(record) = record else {
        return GridEvidence::default();
    };
    let core = record.terminal_core.as_ref();
    let Ok(origin) = scrollback_origin(core, record.cell_emit.scrollback_origin) else {
        omissions.push(TerminalCaptureOmission {
            kind: TerminalCaptureOmissionKind::Rows,
            name: "worker.history_rows".to_owned(),
            reason: TerminalCoverageReason::CoreExportUnavailable,
            dropped_count: 0,
            dropped_bytes: 0,
            range: None,
        });
        return GridEvidence::default();
    };
    let total = origin + core.scrollback_count() as u64;
    let tail_rows = TERMINAL_CAPTURE_LIMITS.core_scrollback_tail_rows as u64;
    let tail_start = origin.max(total.saturating_sub(tail_rows));
    let tail = read_scrollback_range(core, tail_start, total, origin);
    let (history_rows, ranges) = read_history_ranges(core, origin, total, requested);
    GridEvidence {
        readable: true,
        cols: u32::from(core.cols()),
        rows: u32::from(core.rows()),
        origin,
        total,
        tail,
        history_rows,
        ranges,
    }
}

fn range(start: u64, end: u64, status: Status, rows: u64) -> TerminalCaptureHistoryRange {
    TerminalCaptureHistoryRange {
        start: start.to_string(),
        end: end.to_string(),
        status,
        rows,
    }
}

/// The requested ranges (or, with none, the newest `capture_history_rows`),
/// read under one shared row budget.
fn read_history_ranges(
    core: &dyn TerminalCore,
    origin: u64,
    total: u64,
    requested: &[HistoryRequest],
) -> (Vec<CellRow>, Vec<TerminalCaptureHistoryRange>) {
    let history_rows = TERMINAL_CAPTURE_LIMITS.capture_history_rows as u64;
    let mut rows = Vec::new();
    let mut ranges = Vec::new();
    let mut budget = history_rows;
    let mut wanted = requested.to_vec();
    if wanted.is_empty() {
        wanted.push(HistoryRequest {
            start: origin.max(total.saturating_sub(history_rows)),
            end: total,
        });
    }
    wanted.sort_by_key(|request| request.start);
    for request in wanted {
        // `worker.history_ranges` is validated against `layer_entries`: stop
        // rather than assemble a bundle the write-side gate would refuse.
        if ranges.len() >= TERMINAL_CAPTURE_LIMITS.layer_entries - 2 {
            break;
        }
        if request.start < origin {
            ranges.push(range(
                request.start,
                request.end.min(origin),
                Status::Evicted,
                0,
            ));
        }
        if request.end > total {
            ranges.push(range(
                request.start.max(total),
                request.end,
                Status::Unavailable,
                0,
            ));
        }
        let start = request.start.max(origin);
        let end = request.end.min(total).min(start + budget);
        if end <= start || budget == 0 {
            continue;
        }
        let page = read_scrollback_range(core, start, end, origin);
        budget = budget.saturating_sub(page.len() as u64);
        ranges.push(range(start, end, Status::Present, page.len() as u64));
        rows.extend(page);
    }
    (rows, ranges)
}
