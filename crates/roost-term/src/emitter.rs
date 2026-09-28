//! Full frame or delta: the decision, and the state it advances.
//!
//! This is the v2 `nextCellFrame`, and its job is to answer one question — can
//! the client reach the current state from what it already holds? Four things
//! answer no, and only one of them is a choice:
//!
//! - the first frame, or an explicit force (attach, rebuild);
//! - a **semantic reframe**: the grid changed shape, the alt screen toggled,
//!   the monotonic total went backwards, or the ring evicted past what the
//!   client holds;
//! - a **live delta** that would append more than
//!   [`LIVE_DELTA_SCROLLBACK_ROWS_CAP`] history rows;
//! - nothing at all, in which case a delta is built from the dirty rows.
//!
//! The fourth case is the one that looks optional and is not. A delta's append
//! starts at the retained floor, so a client that fell further behind than the
//! ring's capacity would silently receive a frame with a hole in its history
//! and no way to notice. Capping the live append trades a periodic full frame
//! for the certainty that no client is ever more than 250 lines behind.

use roost_protocol::cell::CellGridFrame;

use crate::core::TerminalCore;
use crate::error::TerminalCoreResult;
use crate::frame::{grid_delta_frame, grid_to_cell_frame, scrollback_origin};

/// The most history rows one live delta may append before it becomes a full
/// frame instead.
pub const LIVE_DELTA_SCROLLBACK_ROWS_CAP: u64 = 250;

/// Per-channel emit state. One of these per session; the core is not owned by
/// it and the frame it builds does not mutate the core.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellEmitState {
    /// The coordinator-minted generation every emitted frame is addressed by.
    pub stream_id: String,
    /// The base of this worker-side grid numbering.
    pub grid_epoch_base: String,
    /// Increments only when a non-forced semantic reframe invalidates row
    /// identity, so a client can tell a new grid from a new snapshot of the
    /// same one.
    pub grid_epoch_revision: u64,
    /// The last emitted sequence number.
    pub seq: u64,
    /// The last emitted scrollback total, in monotonic index space.
    pub last_scrollback_total: u64,
    pub sent_full: bool,
    pub cols: u16,
    pub rows: u16,
    pub alt: bool,
    /// The absolute scrollback origin across an explicit worker adoption, where
    /// keeper history creates a replacement core. Zero for the core a session
    /// spawns with.
    pub scrollback_origin: u64,
    /// The eviction origin at the last emit, read once so a frame's `sb_base`,
    /// `scrollback_total` and append range all describe the same observation of
    /// the ring.
    pub sb_dropped: u64,
}

impl CellEmitState {
    /// A fresh state for a core that has just been adopted.
    pub fn new(grid_epoch_base: impl Into<String>, stream_id: impl Into<String>) -> Self {
        Self {
            stream_id: stream_id.into(),
            grid_epoch_base: grid_epoch_base.into(),
            grid_epoch_revision: 0,
            seq: 0,
            last_scrollback_total: 0,
            sent_full: false,
            cols: 0,
            rows: 0,
            alt: false,
            scrollback_origin: 0,
            sb_dropped: 0,
        }
    }

    /// The grid epoch this emit will stamp on its frame.
    pub fn grid_epoch(&self) -> String {
        format!("{}:{}", self.grid_epoch_base, self.grid_epoch_revision)
    }
}

/// The next frame, and the state that produced it.
///
/// `force` is the caller's claim that the client must be given a complete
/// grid. It does not advance the grid epoch: a forced snapshot of an unchanged
/// grid is the same grid, and bumping the epoch would invalidate every row
/// index the client holds for nothing.
pub fn next_cell_frame(
    core: &dyn TerminalCore,
    state: &CellEmitState,
    force: bool,
    tail_rows: Option<u32>,
) -> TerminalCoreResult<(CellGridFrame, CellEmitState)> {
    let cols = core.cols();
    let rows = core.rows();
    let retained = core.scrollback_count() as u64;
    let alt = core.using_alt_screen();
    // Read once: a frame's base, total and append range have to describe the
    // same observation of the ring, and a ring that evicted between two reads
    // would otherwise make the frame describe itself inconsistently.
    let sb_dropped = scrollback_origin(core, state.scrollback_origin)?;
    let mono_total = sb_dropped + retained;

    let semantic_reframe = !force
        && state.sent_full
        && (cols != state.cols
            || rows != state.rows
            || alt != state.alt
            // A vertical grow pops rows back OUT of history into the viewport
            // without discarding anything, so the monotonic total shrinks; so
            // does a reset.
            || mono_total < state.last_scrollback_total
            // The ring evicted PAST what the client holds — a whole ring's
            // worth of lines inside one coalesce window. A delta's append starts
            // at the retained floor, so [last_total, sb_dropped) would never
            // reach the client and its history would splice a hole. Those
            // lines are gone from the ring either way; an honest reframe is the
            // only truthful frame left.
            || sb_dropped > state.last_scrollback_total);

    let live_delta_exceeds_cap = !force
        && state.sent_full
        && !semantic_reframe
        && mono_total.saturating_sub(state.last_scrollback_total) > LIVE_DELTA_SCROLLBACK_ROWS_CAP;

    let reframe = force || !state.sent_full || semantic_reframe || live_delta_exceeds_cap;
    let grid_epoch_revision = state.grid_epoch_revision + u64::from(semantic_reframe);
    let grid_epoch = format!("{}:{}", state.grid_epoch_base, grid_epoch_revision);
    let seq = state.seq + 1;

    // A capped live checkpoint is viewport-only: it exists to stop a client
    // falling behind, and carrying its history would defeat the cap.
    let history_budget = if live_delta_exceeds_cap {
        Some(0)
    } else {
        tail_rows
    };
    let frame = if reframe {
        grid_to_cell_frame(
            core,
            seq,
            &grid_epoch,
            &state.stream_id,
            history_budget,
            sb_dropped,
        )
    } else {
        grid_delta_frame(
            core,
            state.last_scrollback_total,
            seq,
            state.seq,
            &grid_epoch,
            &state.stream_id,
            sb_dropped,
        )
    };

    let next = CellEmitState {
        stream_id: state.stream_id.clone(),
        grid_epoch_base: state.grid_epoch_base.clone(),
        grid_epoch_revision,
        seq,
        last_scrollback_total: mono_total,
        sent_full: true,
        cols,
        rows,
        alt,
        scrollback_origin: state.scrollback_origin,
        sb_dropped,
    };
    Ok((frame, next))
}
