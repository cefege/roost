//! Bounded, demand-driven scrollback retrieval from the worker's
//! authoritative grid. Owned by the worker.
//!
//! This is the ONE reader. The coordinator RPC and the local terminal socket
//! both wrap it, so neither transport owns a second traversal of the cell codec
//! nor a second set of epoch and control fences. That is why the fence lives
//! here: a second reader would be a second chance to splice two grids.
//!
//! Two bounds, and they answer different questions.
//!
//! THE PAGE CEILING bounds the response. It bounds the per-cell walk and the
//! JSON that comes back for one request; the client chunks below it and issues
//! several per wave, so this is the per-request cap and not the per-reveal cost.
//!
//! THE SLICE BOUNDS THE WALK. Every OTHER session's PTY output on this worker
//! is stalled for the duration of one slice, so a slice has to stay well under
//! a frame. A page is therefore read in several slices, and the reader is
//! cancellable between them — which is only safe because the epoch fence makes
//! a cancelled read resumable-or-abandoned, never silently spliced.

use std::time::Duration;

/// The server-side ceiling on rows per response.
///
/// The client chunks at 1000 and issues `BACKFILL_CONCURRENCY` of those per
/// wave, so this is a per-request cap rather than the cost of a reveal.
pub const SCROLLBACK_MAX_ROWS_PER_PAGE: u32 = 2_000;

/// Rows read per event-loop slice.
///
/// Every other session's PTY output on this worker is blocked for one slice, so
/// this stays well under a frame. It matches the client's per-frame splice
/// budget so the two do not disagree about what a frame is.
pub const SCROLLBACK_SLICE_ROWS: u32 = 250;

/// Why a read was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The caller's grid epoch is not the worker's current one.
    ///
    /// The grid was replaced between the client asking and the worker
    /// answering, so the rows it would have received describe a grid that no
    /// longer exists. Serving them would splice two grids into one scrollback
    /// and the splice would be invisible.
    StaleEpoch,
    /// A zero-width request. Not an error worth a round trip, and answering it
    /// with an empty page would look like a successful read of nothing.
    EmptyRequest,
    /// The transport budget refused the whole read.
    BudgetRefused,
}

/// The identity of the grid a read is bound to.
///
/// An EMPTY epoch on a request binds the read to whatever the worker currently
/// has and returns it, which is how a first read establishes the binding.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EpochBinding {
    current: String,
}

impl EpochBinding {
    pub fn new(current: impl Into<String>) -> Self {
        Self {
            current: current.into(),
        }
    }

    pub fn current(&self) -> &str {
        &self.current
    }

    /// The grid is replaced, so every outstanding read is now against a grid
    /// that no longer exists.
    pub fn replace(&mut self, current: impl Into<String>) {
        self.current = current.into();
    }
}

/// A page of scrollback rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// Inclusive first row, and the row one past the last.
    pub start_row: u32,
    pub end_row: u32,
    /// How many rows the grid holds in total, which may exceed the page.
    pub total: u32,
    pub cols: u16,
    /// The epoch this page is bound to. The client keeps it and presents it on
    /// the next request, which is what makes a gap between pages detectable.
    pub grid_epoch: String,
    /// Whether more rows remain after this page.
    pub has_more: bool,
}

impl Page {
    /// How many rows this page actually carries.
    pub fn row_count(&self) -> u32 {
        self.end_row.saturating_sub(self.start_row)
    }
}

/// A read, asked for and bounded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The epoch the client believes it is reading. Empty binds to the
    /// worker's current epoch and adopts it.
    pub grid_epoch: String,
    /// The last row the client wants, exclusive. The client walks backwards
    /// from the end of the scrollback, so this is an upper bound.
    pub end_row: u32,
    /// How many rows the client would like, bounded by the server ceiling.
    pub max_rows: u32,
}

/// Compute the page a request names, or refuse it.
///
/// The window is `[max(0, end - max), end)`, clamped to the grid and to the
/// page ceiling. Clamping rather than refusing on an over-large `max_rows` is
/// deliberate: the client is allowed to ask for more than one page and gets a
/// page, because the alternative is a failure for a request that was
/// well-formed.
pub fn page_for(
    request: &Request,
    total: u32,
    cols: u16,
    binding: &EpochBinding,
) -> Result<Page, Refusal> {
    if !request.grid_epoch.is_empty() && request.grid_epoch != binding.current {
        return Err(Refusal::StaleEpoch);
    }
    if request.max_rows == 0 {
        return Err(Refusal::EmptyRequest);
    }

    let end_row = request.end_row.min(total);
    let start_row = end_row.saturating_sub(request.max_rows.min(SCROLLBACK_MAX_ROWS_PER_PAGE));
    Ok(Page {
        start_row,
        end_row,
        total,
        cols,
        grid_epoch: binding.current.clone(),
        has_more: start_row > 0,
    })
}

/// How many slices a page of this size takes.
///
/// Exposed because the client budgets its backfill wave by it: a page that
/// silently became ten slices would stall every other session ten times.
pub fn slice_count(rows: u32) -> u32 {
    rows.div_ceil(SCROLLBACK_SLICE_ROWS).max(1)
}

/// The walk over one page, cancellable between slices.
///
/// `admit_row` is the transport budget, checked BEFORE a row is retained, so a
/// row that was never taken is never charged for. `continue_read` is live
/// authority: a direct read stops the moment the socket is closed or the grant
/// revoked, and stops BETWEEN slices rather than mid-row, so a page is never
/// left half-taken with no way to tell.
pub fn walk_page<F, C>(page: &Page, mut admit_row: F, mut continue_read: C) -> WalkOutcome
where
    F: FnMut(u32) -> bool,
    C: FnMut() -> bool,
{
    let mut taken = Vec::new();
    let mut offset = 0u32;
    loop {
        if !continue_read() {
            return WalkOutcome::Cancelled { taken };
        }
        let slice_end = (offset + SCROLLBACK_SLICE_ROWS).min(page.row_count());
        if offset >= slice_end {
            break;
        }
        let mut refused_in_slice = false;
        while offset < slice_end {
            if !admit_row(offset) {
                refused_in_slice = true;
                break;
            }
            taken.push(offset);
            offset += 1;
        }
        if refused_in_slice {
            return WalkOutcome::BudgetRefused { taken };
        }
    }
    WalkOutcome::Complete { taken }
}

/// What a walk produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WalkOutcome {
    /// Every row was taken.
    Complete { taken: Vec<u32> },
    /// The transport budget refused partway. `taken` is what WAS retained, and
    /// the rest was never charged for.
    BudgetRefused { taken: Vec<u32> },
    /// Live authority ended the read. `taken` is a PARTIAL page, and the caller
    /// must not present it as a complete one.
    Cancelled { taken: Vec<u32> },
}

impl WalkOutcome {
    pub fn taken(&self) -> &[u32] {
        match self {
            WalkOutcome::Complete { taken }
            | WalkOutcome::BudgetRefused { taken }
            | WalkOutcome::Cancelled { taken } => taken,
        }
    }

    /// Whether the page was fully delivered. A caller presenting a partial page
    /// as complete is how a client ends up with a scrollback that has a silent
    /// hole in it.
    pub fn is_complete(&self) -> bool {
        matches!(self, WalkOutcome::Complete { .. })
    }
}

/// How long one slice may block other sessions. Not used to schedule anything
/// here; stated so the two budgets cannot be changed independently and left
/// disagreeing about what a frame is.
pub const SLICE_BUDGET: Duration = Duration::from_millis(8);
