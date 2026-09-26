//! Scrollback retrieval: the page window, the epoch fence, and the cancellable
//! walk. These are bounds and a fence, and both are the kind of arithmetic that
//! is correct until one edge case moves it.

use std::time::Duration;

use roost_worker::scrollback_read::{
    EpochBinding, Refusal, Request, SCROLLBACK_MAX_ROWS_PER_PAGE, SCROLLBACK_SLICE_ROWS,
    WalkOutcome, page_for, slice_count, walk_page,
};

fn request(epoch: &str, end_row: u32, max_rows: u32) -> Request {
    Request {
        grid_epoch: epoch.to_string(),
        end_row,
        max_rows,
    }
}

fn binding() -> EpochBinding {
    EpochBinding::new("epoch-1")
}

/// THE FENCE. The grid was replaced between the client asking and the worker
/// answering, so the rows it would receive describe a grid that no longer
/// exists — and the splice would be invisible.
#[test]
fn a_read_against_a_replaced_grid_is_refused() {
    let mut binding = binding();
    let first = page_for(&request("epoch-1", 500, 100), 1_000, 80, &binding).expect("bound");

    // The grid is replaced — a resize, a repair, a reattach.
    binding.replace("epoch-2");

    assert_eq!(
        page_for(&request("epoch-1", 500, 100), 1_000, 80, &binding),
        Err(Refusal::StaleEpoch),
        "rows from the previous grid are not served, because splicing two \\
         grids produces a scrollback with a hole nobody can see"
    );
    // And the page the client was holding is bound to the epoch it came from.
    assert_eq!(
        first.grid_epoch, "epoch-1",
        "so the client can detect exactly this"
    );
}

/// An EMPTY epoch binds the read to the worker's current one and adopts it.
/// That is how a first read establishes the binding at all.
#[test]
fn an_empty_epoch_binds_to_the_current_one_and_adopts_it() {
    let binding = binding();
    let page = page_for(&request("", 200, 50), 1_000, 80, &binding).expect("bound");
    assert_eq!(
        page.grid_epoch, "epoch-1",
        "and the page says which grid it came from"
    );
}

/// The page is the window ENDED at `end_row`, because a client walking back
/// through scrollback asks for the rows before the one it has.
#[test]
fn a_page_is_the_window_ending_at_the_requested_row() {
    let page = page_for(&request("", 500, 100), 1_000, 80, &binding()).expect("bound");
    assert_eq!((page.start_row, page.end_row), (400, 500));
    assert_eq!(page.row_count(), 100);
    assert_eq!(
        page.total, 1_000,
        "and the grid's full size, which the page is a window onto"
    );
}

/// A request past the end of the grid is CLAMPED, not refused. The client is
/// walking backwards and may not know how much scrollback exists yet.
#[test]
fn a_request_past_the_end_is_clamped_to_the_grid() {
    let page = page_for(&request("", 5_000, 100), 1_000, 80, &binding()).expect("clamped");
    assert_eq!(page.end_row, 1_000, "the read stops at the last row");
    assert_eq!(
        (page.start_row, page.end_row),
        (900, 1_000),
        "and still returns the rows asked for"
    );
    assert!(
        page.has_more,
        "900 rows remain before it, and the client has to be told so or its          backfill stops early and leaves a hole"
    );
}

/// An over-large `max_rows` is CLAMPED to the page ceiling rather than
/// refused. The client is allowed to ask for more than one page; failing a
/// well-formed request would be worse than serving a page of it.
#[test]
fn an_oversized_request_is_clamped_to_the_page_ceiling() {
    let page = page_for(&request("", 10_000, 1_000_000), 10_000, 80, &binding()).expect("clamped");
    assert_eq!(
        page.row_count(),
        SCROLLBACK_MAX_ROWS_PER_PAGE,
        "one page, not a million rows"
    );
    assert!(page.has_more, "and the client is told more remain");
}

/// A zero-width request is refused rather than answered with an empty page,
/// which would look like a successful read of nothing.
#[test]
fn a_zero_width_request_is_refused_rather_than_answered_empty() {
    assert_eq!(
        page_for(&request("", 500, 0), 1_000, 80, &binding()),
        Err(Refusal::EmptyRequest)
    );
}

/// `has_more` is how a client knows to ask again, and it must be exact at the
/// boundary: a page ending at the start of the grid has nothing before it.
#[test]
fn has_more_is_exact_at_the_boundary() {
    let whole = page_for(&request("", 1_000, 1_000), 1_000, 80, &binding()).expect("bound");
    assert!(!whole.has_more, "the whole scrollback in one page");
    assert_eq!((whole.start_row, whole.end_row), (0, 1_000));

    let partial = page_for(&request("", 1_000, 999), 1_000, 80, &binding()).expect("bound");
    assert_eq!((partial.start_row, partial.end_row), (1, 1_000));
}

/// A page spanning several slices says so, because the client budgets its
/// backfill wave by it — a page that silently became ten slices would stall
/// every other session ten times.
#[test]
fn a_page_reports_how_many_slices_it_takes() {
    assert_eq!(
        slice_count(SCROLLBACK_SLICE_ROWS),
        1,
        "exactly one slice is one slice"
    );
    assert_eq!(
        slice_count(SCROLLBACK_SLICE_ROWS + 1),
        2,
        "one row over is a second slice"
    );
    assert_eq!(slice_count(1), 1, "a small page is still one slice");
    assert!(
        slice_count(SCROLLBACK_MAX_ROWS_PER_PAGE) > 1,
        "a full page is several slices"
    );
}

/// The happy path: every row is taken, in order, once.
#[test]
fn a_walk_takes_every_row_in_order() {
    let page = page_for(&request("", 1_000, 500), 1_000, 80, &binding()).expect("bound");
    let outcome = walk_page(&page, |_| true, || true);
    assert!(outcome.is_complete());
    assert_eq!(outcome.taken().len(), 500);
    let taken = outcome.taken();
    assert_eq!(taken[0], 0);
    assert_eq!(taken[499], 499, "and they are consecutive, not a sample");
    assert!(taken.windows(2).all(|pair| pair[1] == pair[0] + 1));
}

/// The transport budget is checked BEFORE a row is retained, so a row that was
/// never taken is never charged for.
#[test]
fn a_refused_row_is_never_charged_for() {
    let page = page_for(&request("", 1_000, 500), 1_000, 80, &binding()).expect("bound");
    let mut charged = 0u32;
    let outcome = walk_page(
        &page,
        |_| {
            charged += 1;
            charged <= 10
        },
        || true,
    );
    assert_eq!(
        outcome,
        WalkOutcome::BudgetRefused {
            taken: (0..10).collect()
        }
    );
    assert_eq!(
        charged, 11,
        "the eleventh was offered and refused, so the caller knows it was not kept"
    );
    assert!(!outcome.is_complete());
}

/// Live authority ends a read BETWEEN SLICES, and a cancelled page is
/// explicitly not a complete one — a caller presenting it as complete is how a
/// client ends up with a scrollback that has a silent hole in it.
#[test]
fn a_cancelled_walk_is_not_a_complete_page() {
    let page = page_for(&request("", 1_000, 1_000), 1_000, 80, &binding()).expect("bound");
    let mut slices = 0u32;
    let outcome = walk_page(
        &page,
        |_| true,
        || {
            slices += 1;
            // Let one full slice through, then end it.
            slices <= 1
        },
    );
    assert!(
        !outcome.is_complete(),
        "a partial page must never look complete"
    );
    assert!(
        outcome.taken().len() <= SCROLLBACK_SLICE_ROWS as usize,
        "and it stops at a slice boundary, not mid-row: {} rows",
        outcome.taken().len()
    );
}

/// A read that was refused by the budget leaves the grid's own bookkeeping
/// untouched, which is what makes a retry possible.
#[test]
fn a_refused_walk_can_be_retried_whole() {
    let page = page_for(&request("", 1_000, 300), 1_000, 80, &binding()).expect("bound");
    let refused = walk_page(&page, |_| false, || true);
    assert!(!refused.is_complete());

    let retried = walk_page(&page, |_| true, || true);
    assert!(retried.is_complete());
    assert_eq!(
        retried.taken().len(),
        300,
        "and the retry takes the whole page"
    );
}

/// The two budgets are stated together so they cannot be changed apart and
/// left disagreeing about what a frame is.
#[test]
fn the_slice_budget_and_the_slice_size_agree() {
    let slice = Duration::from_millis(8);
    assert_eq!(SCROLLBACK_SLICE_ROWS, 250, "250 rows inside an 8ms slice");
    assert!(
        slice <= Duration::from_millis(16),
        "and a slice stays well under a frame at 60fps"
    );
}
