//! One bounded page out of a session's authoritative grid, requested on
//! demand. Owned by the worker.
//!
//! The page arithmetic is NOT here. `crate::scrollback_read` is the one reader
//! in this crate — the same bounds, the same clamping and the same slice walk
//! the local door's scrollback socket uses — and a second copy of the window
//! arithmetic would be a second answer to "which rows does this request name",
//! which is the question the epoch exists to keep singular.
//!
//! What is here is the part a frame adds: reading the request off the wire,
//! classifying a page that came back SHORT, and answering with the floor named
//! so a client stops paging instead of retrying forever.
//!
//! THE FLOOR IS A FACT ABOUT THE PAGE, NOT ABOUT THE REQUEST. A page clamped
//! at the retained edge says which edge it hit. A page that served everything
//! it was asked for says none, even when the session has lost history further
//! back — a client must not be told "you have reached the beginning" when it
//! has not.
//! Ports v2 `apps/worker/src/browser-commands/browser-command-terminal.ts`.

use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};
use crate::scrollback_read;
use crate::session::retained_grid::CellRowJson;

/// What a session's grid looks like to a reader, taken once per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridDescription {
    /// The grid identity every outstanding read is checked against.
    pub binding: scrollback_read::EpochBinding,
    /// How many rows have been dropped from the front, so a request naming a
    /// row below this names one the core no longer holds.
    pub retained_floor: u32,
    /// Where a resize-forced replay begins, or zero when no replay has ever
    /// bounded this session's history.
    ///
    /// Distinct from the retained floor because they look identical from the
    /// outside and mean opposite things: one is "your history aged out", the
    /// other is "a resize re-projected what you had".
    pub resize_replay_floor: u32,
    /// Every row the grid holds, dropped and retained together.
    pub total: u32,
    pub cols: u16,
    /// The live viewport's height.
    ///
    /// Separate from `total` because the newest row a caller may name is one
    /// past the last history row PLUS the viewport, and a description without
    /// it can only say where history ends — which is the row a search starts
    /// from, not the one it may scan up to.
    pub viewport_rows: u16,
}

/// A session's retained grid, as a page request reads it.
pub trait RetainedGrid: Send + Sync {
    /// The grid this session currently has, or the refusal for a session this
    /// worker does not hold.
    fn describe(&self, session_id: SessionId) -> Boxed<Result<GridDescription, Refusal>>;

    /// One row by its absolute index, or `None` when the grid no longer holds
    /// it.
    ///
    /// `session::retained_grid::CellRowJson` and NOT `serde_json::Value`,
    /// because the field ORDER is part of what the browser parses and a `Value`
    /// cannot promise it: `serde_json::Map` is a `BTreeMap` unless the
    /// `preserve_order` feature is switched on for the whole dependency graph.
    /// Returning the row's own serialiser puts the order in a type, so a call
    /// site cannot re-serialise through a map and lose it without the compiler
    /// objecting.
    fn row(&self, session_id: SessionId, absolute_row: u32) -> Boxed<Option<CellRowJson>>;
}

/// Run the one command this owns.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    let grid = deps.grid.as_ref();
    let request_id = command.request_id.as_str();
    let ClientControlFrame::GetScrollbackCells {
        session_id,
        grid_epoch,
        end_row,
        max_rows,
        ..
    } = &command.frame
    else {
        return Err(Refusal::failed(
            "get-scrollback-cells",
            format!("{} is not a scrollback page request", command.frame.kind()),
        ));
    };
    let description = grid.describe(session_id.clone()).await?;
    // The window is CLAMPED, not validated, and the clamp is what the protocol
    // promises. A reader that walks backwards from the end of the scrollback
    // asks for `Number.MAX_SAFE_INTEGER` because it does not know the total
    // yet — the smoke harness's retained-marker scan does exactly that — and the
    // answer is the last page the grid holds. Narrowing to `u32` BEFORE the
    // clamp refused that sentinel, so every "scan the whole retained
    // scrollback" reader failed with `end_row does not fit` and never reached
    // `page_for`, whose whole job is to answer it.
    //
    // So the narrowing happens against the grid's own range, where it cannot
    // fail. A NEGATIVE index is still refused: no clamp has a meaning for it.
    let total = description.total;
    let end_row = match *end_row {
        row if row < 0 => {
            return Err(Refusal::failed(
                "get-scrollback-cells",
                "end_row is negative",
            ));
        }
        row => u32::try_from(row.min(i64::from(total))).unwrap_or(total),
    };
    let max_rows = match *max_rows {
        rows if rows < 0 => {
            return Err(Refusal::failed(
                "get-scrollback-cells",
                "max_rows is negative",
            ));
        }
        // Zero is NOT refused here: `page_for` owns that refusal, with its own
        // reason, and two answers to one empty request is one too many.
        rows => u32::try_from(rows).unwrap_or(u32::MAX),
    };
    let request = scrollback_read::Request {
        grid_epoch: grid_epoch.clone(),
        end_row,
        max_rows,
    };
    let page = scrollback_read::page_for(
        &request,
        description.total,
        description.cols,
        &description.binding,
    )
    .map_err(|refusal| Refusal::failed("get-scrollback-cells", text(&refusal)))?;

    // The window the caller ASKED for, before the retained floor clamped it, and
    // before the PAGE CEILING — which is the half that was wrong.
    //
    // `page_for` clamps `max_rows` to `SCROLLBACK_MAX_ROWS_PER_PAGE` (2 000) and
    // derives `page.start_row` from the clamped value. Re-deriving the wanted
    // start from the RAW request asked for a window thousands of rows lower
    // than the one about to be served, so a page that served everything and
    // never reached the retained edge could still report `history_floor:
    // Evicted` — which is exactly what the floor's own contract forbids, and
    // what tells a client to stop paging at a floor it never hit. The direct
    // reader (`local_terminal/scrollback.rs`) has always taken the window's real
    // start row; this path now does the same.
    //
    // Comparing against the window rather than against the page is also what
    // distinguishes "served everything" from "stopped at the edge".
    let wanted_start = page.start_row;
    let history_floor = history_floor_for(&description, wanted_start);
    let window = page.clamped_to_floor(description.retained_floor);

    // A page's rows ARE `[start_row, end_row)`, and the walk is synchronous
    // against a description taken once, so the two cannot disagree here. They
    // CAN disagree about the grid: the ring keeps evicting while a saturated
    // session is fed, so a row this window named can be gone by the time it is
    // read. A row that reads absent STOPS the page — it is never skipped,
    // because a skipped row is a hole the client splices over and cannot see.
    let walk = scrollback_read::walk_page(&window, |_| true, || true);
    let mut rows = Vec::with_capacity(window.row_count() as usize);
    let mut served_end = window.start_row;
    for offset in walk.taken() {
        let absolute = window.start_row + offset;
        let Some(row) = grid.row(session_id.clone(), absolute).await else {
            tracing::debug!(
                session_id = %session_id,
                start_row = window.start_row,
                lost_row = absolute,
                "a scrollback page stopped at a row the grid no longer holds"
            );
            break;
        };
        rows.push(row);
        served_end = absolute + 1;
    }
    let page = window.with_end(served_end);
    tracing::debug!(
        session_id = %session_id,
        start_row = page.start_row,
        end_row = page.end_row,
        rows = rows.len(),
        history_floor = history_floor.as_wire(),
        "a scrollback page was served"
    );
    Ok(Answered::Reply(Reply::ok(
        request_id,
        serde_json::json!({
            "rows": rows,
            "cols": page.cols,
            "total": page.total,
            "start_row": page.start_row,
            "end_row": page.end_row,
            "grid_epoch": page.grid_epoch,
            "history_floor": history_floor,
        }),
    )))
}

/// Which edge a short page stopped at, or none when it stopped at neither.
///
/// A replay floor and a retained floor look identical from outside and mean
/// opposite things — one is "your history aged out", the other is "a resize
/// re-projected what you had" — so the one the page actually reached is the
/// one named. A page that served everything it asked for names neither, even
/// when the session has lost history further back.
pub fn history_floor_for(
    description: &GridDescription,
    wanted_start: u32,
) -> ScrollbackHistoryFloor {
    if description.retained_floor == 0 || wanted_start >= description.retained_floor {
        return ScrollbackHistoryFloor::None;
    }
    let replay = description.resize_replay_floor;
    if replay > 0 && description.retained_floor == replay {
        return ScrollbackHistoryFloor::ResizeReplay;
    }
    ScrollbackHistoryFloor::Evicted
}

/// The reader's own refusal, as the message a caller is answered with.
fn text(refusal: &scrollback_read::Refusal) -> String {
    match refusal {
        scrollback_read::Refusal::StaleEpoch => "grid epoch changed".to_owned(),
        scrollback_read::Refusal::EmptyRequest => "the request named no rows".to_owned(),
        scrollback_read::Refusal::BudgetRefused => {
            "the transport budget refused the read".to_owned()
        }
    }
}
