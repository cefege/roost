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

use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};
use crate::scrollback_read;

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
}

/// A session's retained grid, as a page request reads it.
pub trait RetainedGrid: Send + Sync {
    /// The grid this session currently has, or the refusal for a session this
    /// worker does not hold.
    fn describe(&self, session_id: SessionId) -> Boxed<Result<GridDescription, Refusal>>;

    /// One row by its absolute index, or `None` when the grid no longer holds
    /// it. Already in the wire shape the browser parses: the cell row's JSON
    /// projection belongs beside `roost_protocol::cell::proto`, and a copy
    /// written here would be a second answer to how a row is spelled.
    fn row(&self, session_id: SessionId, absolute_row: u32) -> Boxed<Option<serde_json::Value>>;
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
    let request = scrollback_read::Request {
        grid_epoch: grid_epoch.clone(),
        end_row: u32::try_from(*end_row)
            .map_err(|_| Refusal::failed("get-scrollback-cells", "end_row does not fit"))?,
        max_rows: u32::try_from(*max_rows)
            .map_err(|_| Refusal::failed("get-scrollback-cells", "max_rows does not fit"))?,
    };
    let page = scrollback_read::page_for(
        &request,
        description.total,
        description.cols,
        &description.binding,
    )
    .map_err(|refusal| Refusal::failed("get-scrollback-cells", text(&refusal)))?;

    // The window the caller ASKED for, before the ceiling and the retained
    // floor clamped it. Comparing against this rather than against the page is
    // what distinguishes "served everything" from "stopped at the edge".
    let wanted_start = page.end_row.saturating_sub(request.max_rows);
    let history_floor = history_floor_for(&description, wanted_start);

    // The walk is synchronous against a description taken once, so a reframe
    // cannot interleave inside it: there is no await between reading the grid
    // and finishing the page. The epoch on the answer is what a client uses to
    // notice a reframe that happened after this point.
    let walk = scrollback_read::walk_page(&page, |_| true, || true);
    let mut rows = Vec::with_capacity(page.row_count() as usize);
    for offset in walk.taken() {
        let absolute = page.start_row + offset;
        if let Some(row) = grid.row(session_id.clone(), absolute).await {
            rows.push(row);
        }
    }
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
