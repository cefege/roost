//! Terminal state as a `CellGridFrame`: the full snapshot, the delta, and the
//! scrollback range read both of them need.
//!
//! Every index here is **monotonic**. The retained window is
//! `[sb_dropped, sb_dropped + retained)`, and `sb_dropped` is the core's own
//! eviction count rather than anything inferred from retained history — the
//! inference Roost used to do cost about 1200 reads per emit near the
//! saturation point, went blind past a 256-line scan window, and could alias
//! two identical tails, and each of those failures silently re-aliased every
//! absolute row index a client held.

use std::sync::Arc;

use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan};

use crate::core::TerminalCore;
use crate::error::TerminalCoreResult;
use crate::row_spans::row_to_spans;

/// One viewport row's spans.
pub fn viewport_row_spans(core: &dyn TerminalCore, row: u16, cols: u16) -> Arc<[CellSpan]> {
    let cells: Vec<_> = (0..cols).map(|col| core.viewport_cell(row, col)).collect();
    Arc::from(row_to_spans(&cells, cells.len()))
}

/// One retained line's spans, addressed by the core's newest-first offset.
///
/// The line's own stored width bounds the read, never the current viewport
/// width: a history line keeps the width it was written at and can be wider.
pub fn scrollback_offset_spans(core: &dyn TerminalCore, offset: usize) -> Arc<[CellSpan]> {
    let length = core.scrollback_line_len(offset);
    let cells: Vec<_> = (0..length)
        .map(|col| core.scrollback_cell(offset, col as u16))
        .collect();
    Arc::from(row_to_spans(&cells, cells.len()))
}

/// One retained line, addressed by its monotonic absolute index.
fn scrollback_row(
    core: &dyn TerminalCore,
    absolute: u64,
    retained: u64,
    sb_dropped: u64,
) -> CellRow {
    // Oldest-first absolute index to the core's newest-first offset.
    let offset = retained
        .saturating_sub(1)
        .saturating_sub(absolute - sb_dropped) as usize;
    CellRow {
        index: u32::try_from(absolute).unwrap_or(u32::MAX),
        spans: scrollback_offset_spans(core, offset),
    }
}

/// The scalar state every frame carries, read once.
fn scalar_state(
    core: &dyn TerminalCore,
    seq: u64,
    grid_epoch: &str,
    stream_id: &str,
    sb_dropped: u64,
    base_seq: u64,
    full: bool,
) -> CellGridFrame {
    let cursor = core.cursor();
    let mut frame = CellGridFrame {
        stream_id: stream_id.to_owned(),
        grid_epoch: grid_epoch.to_owned(),
        cols: u32::from(core.cols()),
        rows: u32::from(core.rows()),
        cursor_row: u32::from(cursor.row),
        cursor_col: u32::from(cursor.col),
        cursor_visible: cursor.visible,
        alt_screen: core.using_alt_screen(),
        cursor_keys_app: core.cursor_keys_app(),
        bracketed_paste: core.bracketed_paste(),
        mouse_tracking: core.mouse_tracking(),
        mouse_sgr: core.mouse_sgr(),
        focus_events: core.focus_events(),
        full,
        viewport_rows: Vec::new(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: sb_dropped + core.scrollback_count() as u64,
        sb_base: 0,
        base_seq,
        seq,
    };
    frame.viewport_rows = (0..core.rows())
        .map(|row| CellRow {
            index: u32::from(row),
            spans: viewport_row_spans(core, row, core.cols()),
        })
        .collect();
    frame
}

/// A full snapshot: the whole viewport, plus the newest `tail_rows` history
/// lines when it is set.
///
/// `Some(0)` is the authoritative viewport-only contract — the caller's "no
/// history in this frame" — while `None` means "a complete grid", which a
/// caller that genuinely wants one can still ask for.
pub fn grid_to_cell_frame(
    core: &dyn TerminalCore,
    seq: u64,
    grid_epoch: &str,
    stream_id: &str,
    tail_rows: Option<u32>,
    sb_dropped: u64,
) -> CellGridFrame {
    let mut frame = scalar_state(core, seq, grid_epoch, stream_id, sb_dropped, 0, true);
    let mono_total = frame.scrollback_total;
    let sb_base = match tail_rows {
        None => sb_dropped,
        Some(tail) => sb_dropped.max(mono_total.saturating_sub(u64::from(tail))),
    };
    frame.sb_base = sb_base;
    let retained = core.scrollback_count() as u64;
    frame.scrollback_rows = (sb_base..mono_total)
        .map(|absolute| scrollback_row(core, absolute, retained, sb_dropped))
        .collect();
    frame
}

/// A delta built from the core's dirty rows and the monotonic-total delta.
///
/// The caller MUST call [`TerminalCore::clear_dirty`] after consuming the
/// frame, or the next delta repeats every row it already sent. Deciding
/// full-versus-delta is the emitter's job, not this function's: send a full
/// frame on attach, resize, alt-screen toggle, or a monotonic-total rewind.
pub fn grid_delta_frame(
    core: &dyn TerminalCore,
    previous_mono_total: u64,
    seq: u64,
    base_seq: u64,
    grid_epoch: &str,
    stream_id: &str,
    sb_dropped: u64,
) -> CellGridFrame {
    let mut frame = scalar_state(
        core, seq, grid_epoch, stream_id, sb_dropped, base_seq, false,
    );
    let cols = core.cols();
    frame.viewport_rows = (0..core.rows())
        .filter(|row| core.is_dirty_row(*row))
        .map(|row| CellRow {
            index: u32::from(row),
            spans: viewport_row_spans(core, row, cols),
        })
        .collect();
    let retained = core.scrollback_count() as u64;
    frame.scrollback_append =
        read_scrollback_range(core, previous_mono_total, sb_dropped + retained, sb_dropped);
    frame
}

/// Read retained lines `[start, end)` by monotonic absolute index, clamped to
/// the retained window. An empty or inverted range reads `[]`.
///
/// The clamp is what makes a delta honest when the ring evicted past what the
/// client holds: the append starts at the retained floor, so the gap the
/// client cannot see is never spliced over with a line that is not adjacent to
/// it. The emitter reframes in that case; this function simply refuses to lie.
pub fn read_scrollback_range(
    core: &dyn TerminalCore,
    start: u64,
    end: u64,
    sb_dropped: u64,
) -> Vec<CellRow> {
    let retained = core.scrollback_count() as u64;
    let low = sb_dropped;
    let high = sb_dropped + retained;
    let start = start.clamp(low, high);
    let end = end.clamp(low, high);
    if end <= start {
        return Vec::new();
    }
    (start..end)
        .map(|absolute| scrollback_row(core, absolute, retained, sb_dropped))
        .collect()
}

/// The eviction origin every absolute history index is measured from.
///
/// A core that cannot report its own discarded-line count is refused, not
/// approximated: the frame that would have carried a wrong index is worse than
/// no frame, because the caller cannot tell them apart.
pub fn scrollback_origin(core: &dyn TerminalCore, base: u64) -> TerminalCoreResult<u64> {
    core.scrollback_origin(base)
}
