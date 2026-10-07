//! The admission gate: whether one arriving frame may ride the pending batch
//! as a sparse delta, or must replace it with a full — the three bounds, the
//! seq-continuity rule and the stale-full fence of
//! `apps/web/src/renderer/terminal-render-scheduler.ts`. Pure functions called
//! by `RenderScheduler`; the batch they judge is `scheduler::frames`.

use std::sync::Arc;

use roost_protocol::cell::{CellGridFrame, CellRow};

use super::frames::PendingRender;

/// Frames one queued sparse batch may carry. A busier pane than this has a
/// browser frame that costs more in DOM writes than the deltas save.
pub(super) const MAX_PENDING_DELTA_FRAMES: usize = 64;

/// Scrollback lines one queued sparse batch may append. Past this the batch
/// paints a full instead, which costs one clone and a full re-render of rows
/// that are mostly unchanged.
pub(super) const MAX_PENDING_SCROLLBACK_ROWS: u64 = 250;

/// Spans one queued sparse batch may carry, so a paste that ships its history
/// as deltas cannot turn one browser frame into megabytes of row shells.
pub(super) const MAX_PENDING_DELTA_SPANS: usize = 65_536;

/// The six facts a sparse batch must agree on, read off either the last frame
/// already queued or the watermark the DOM is reconciled to.
///
/// Both sources are "all six or none", which is why one borrowed view serves
/// both: a partial identity would let a delta continue from a half-known grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct GridIdentity<'a> {
    pub stream_id: &'a str,
    pub grid_epoch: &'a str,
    pub cols: u32,
    pub rows: u32,
    pub alt_screen: bool,
    pub seq: u64,
}

impl<'a> GridIdentity<'a> {
    pub(super) fn of_frame(frame: &'a CellGridFrame) -> Self {
        Self {
            stream_id: &frame.stream_id,
            grid_epoch: &frame.grid_epoch,
            cols: frame.cols,
            rows: frame.rows,
            alt_screen: frame.alt_screen,
            seq: frame.seq,
        }
    }

    pub(super) fn of_reconciled(grid: &'a ReconciledGrid) -> Self {
        Self {
            stream_id: &grid.stream_id,
            grid_epoch: &grid.grid_epoch,
            cols: grid.cols,
            rows: grid.rows,
            alt_screen: grid.alt_screen,
            seq: grid.seq,
        }
    }
}

/// The grid the painted DOM is reconciled to, and the watermark the next sparse
/// delta has to extend.
///
/// `None` until a paint lands: with no baseline there is nothing a delta can be
/// shown to continue from, which is why the first frame a pane ever sees must
/// be applied rather than folded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ReconciledGrid {
    pub stream_id: String,
    pub grid_epoch: String,
    pub cols: u32,
    pub rows: u32,
    pub alt_screen: bool,
    pub seq: u64,
}

impl ReconciledGrid {
    /// The watermark a paint that landed on `canonical` advances to.
    pub(super) fn of_canonical(canonical: &CellGridFrame) -> Self {
        Self {
            stream_id: canonical.stream_id.clone(),
            grid_epoch: canonical.grid_epoch.clone(),
            cols: canonical.cols,
            rows: canonical.rows,
            alt_screen: canonical.alt_screen,
            seq: canonical.seq,
        }
    }
}

/// Whether a `full` claims a position this scheduler has already passed.
///
/// A full naming an OLDER sequence is stale and is dropped outright: admitting
/// it would move the canonical backwards underneath a delta batch that is
/// already queued. A full at the SAME sequence on a DIFFERENT grid is a
/// rebaseline collision — one worker cannot have produced two grids at one
/// sequence — and is dropped for the same reason.
///
/// A full on a different stream is NOT a conflict. The coordinator mints a new
/// stream whenever viewer membership moves, so the first full of a new stream
/// is exactly how a rebaseline arrives.
pub(super) fn full_conflicts_with_known_canonical(
    canonical: &CellGridFrame,
    known: Option<GridIdentity<'_>>,
) -> bool {
    let Some(known) = known else {
        return false;
    };
    if known.stream_id != canonical.stream_id {
        return false;
    }
    if canonical.seq != known.seq {
        return canonical.seq < known.seq;
    }
    known.grid_epoch != canonical.grid_epoch
        || known.cols != canonical.cols
        || known.rows != canonical.rows
        || known.alt_screen != canonical.alt_screen
}

/// Whether a delta extends `previous` exactly: the same grid, and its declared
/// base is the sequence the previous frame reached.
///
/// Continuity is exact on purpose. A gap is not a fast path, and `base_seq` is
/// the frame the delta was computed against, so anything else would fold a
/// delta onto rows that are not the ones it was diffed from.
pub(super) fn delta_follows(previous: GridIdentity<'_>, frame: &CellGridFrame) -> bool {
    previous.stream_id == frame.stream_id
        && previous.grid_epoch == frame.grid_epoch
        && previous.cols == frame.cols
        && previous.rows == frame.rows
        && previous.alt_screen == frame.alt_screen
        && previous.seq == frame.base_seq
}

/// The spans a frame would add to a batch with `limit` of them still free,
/// stopping as soon as the limit is passed.
///
/// The early stop matters: a paste can carry tens of thousands of spans, and
/// counting all of them to learn the batch is over its bound is the cost the
/// bound exists to avoid.
pub(super) fn count_incoming_spans(frame: &CellGridFrame, limit: usize) -> usize {
    let mut span_count = 0usize;
    for row in frame
        .viewport_rows
        .iter()
        .chain(frame.scrollback_append.iter())
    {
        span_count = span_count.saturating_add(row.spans.len());
        if span_count > limit {
            return span_count;
        }
    }
    span_count
}

/// Take a private copy of a delta's row shells.
///
/// A queued delta OUTLIVES the replica that produced it, and the replica keeps
/// folding: a later frame renumbers `viewport_rows` and `scrollback_append` in
/// place. A queued row that shared its index with the replica's would be
/// repainted at the newer coordinate and the grid would hold a row no frame
/// described. The index is copied out; the spans are shared, because they are
/// immutable after decoding and cells a renderer already painted must not be
/// copied to renumber a row.
pub(super) fn own_queued_delta(frame: &CellGridFrame) -> CellGridFrame {
    let mut owned = frame.clone();
    owned.viewport_rows = own_row_shells(&frame.viewport_rows);
    owned.scrollback_append = own_row_shells(&frame.scrollback_append);
    owned
}

fn own_row_shells(rows: &[CellRow]) -> Vec<CellRow> {
    rows.iter()
        .map(|row| CellRow {
            index: row.index,
            spans: Arc::clone(&row.spans),
            mark: row.mark,
        })
        .collect()
}

/// The counters a new delta would give a batch it may extend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DeltaExtend {
    pub appended_rows: u64,
    pub span_count: usize,
}

/// The new batch counters when `frame` may extend `pending`, or `None` when it
/// must be replaced by a full. The rules run in v2's order — foreground, a
/// sparse frame, exact seq continuity, no queued full, the same grid, then the
/// three bounds — and every one is about the BATCH: a delta rides because the
/// run it joins is still contiguous and inside every bound.
pub(super) fn admit_delta(
    foreground: bool,
    pending: Option<&PendingRender>,
    reconciled: Option<&ReconciledGrid>,
    frame: &CellGridFrame,
) -> Option<DeltaExtend> {
    if !foreground || frame.full || frame.base_seq.checked_add(1) != Some(frame.seq) {
        return None;
    }
    let (previous, queued_frames, prior_rows, prior_spans) = match pending {
        Some(PendingRender::Delta {
            deltas,
            appended_rows,
            span_count,
            ..
        }) => (
            GridIdentity::of_frame(deltas.last()?),
            deltas.len(),
            *appended_rows,
            *span_count,
        ),
        // A delta may never join a full: the full already IS the canonical the
        // run would have to fold onto.
        Some(PendingRender::Full { .. }) => return None,
        // With no baseline, nothing shows the delta is a continuation.
        None => (GridIdentity::of_reconciled(reconciled?), 0, 0, 0),
    };
    if !delta_follows(previous, frame) {
        return None;
    }
    let appended = u64::try_from(frame.scrollback_append.len()).unwrap_or(u64::MAX);
    let appended_rows = prior_rows.saturating_add(appended);
    let remaining_spans = MAX_PENDING_DELTA_SPANS.saturating_sub(prior_spans);
    let incoming_spans = count_incoming_spans(frame, remaining_spans);
    if queued_frames + 1 > MAX_PENDING_DELTA_FRAMES
        || appended_rows > MAX_PENDING_SCROLLBACK_ROWS
        || incoming_spans > remaining_spans
    {
        return None;
    }
    Some(DeltaExtend {
        appended_rows,
        span_count: prior_spans.saturating_add(incoming_spans),
    })
}
