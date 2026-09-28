//! Scrollback over a direct terminal carrier: the ONE authoritative reader
//! (`crate::scrollback_read`'s window and walk over `session::retained_grid`'s
//! grid description and rows) wrapped into the `LocalScrollbackResponse` the
//! local page expects. The grant's session set is checked here because this
//! transport authorizes per grant, before the read, between slices and after.
//! Called by `super::controls`. Ports
//! `apps/worker/src/local-door/local-terminal-scrollback.ts`.

use roost_proto::buffa::Message;
use roost_proto::{LocalScrollbackRequest, LocalScrollbackResponse, PbCellRow, ScrollbackHistoryFloor as PbFloor};
use roost_protocol::cell::proto::cell_row_to_proto;
use roost_protocol::terminal_peer::peer::TerminalPeerLaneByteCaps;
use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use roost_protocol::wire::brand::{ChannelId, SessionId};

use crate::browser_commands::Refusal;
use crate::browser_commands::scrollback_page::{GridDescription, history_floor_for};
use crate::scrollback_read::{self, Page, WalkOutcome};
use crate::session::lifecycle::SessionManager;
use crate::session::retained_grid::{describe_grid, row_cells};
use crate::session::table::SessionTable;

const SESSION_UNAVAILABLE: &str = "terminal session is unavailable";
/// The reader works in the page's JSON-safe absolute row space.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const ENVELOPE_HEADROOM_BYTES: usize = 64 * 1024;
const ROWS_MAX_BYTES: usize = TerminalPeerLaneByteCaps::HISTORY - ENVELOPE_HEADROOM_BYTES;

/// Read one page for a direct carrier. `allows` is the port's live session
/// predicate; a page read while it held but answered after it stopped holding
/// carries no rows.
pub async fn read_local_scrollback(
    manager: &SessionManager,
    sessions: &SessionTable,
    request: &LocalScrollbackRequest,
    allows: &(dyn Fn(&str) -> bool + Sync),
) -> LocalScrollbackResponse {
    if !allows(&request.session_id) {
        return failure(request, SESSION_UNAVAILABLE);
    }
    if request.end_row > MAX_SAFE_INTEGER {
        return failure(request, "scrollback end_row is out of range");
    }
    let page = match read_page(manager, sessions, request, allows).await {
        Ok(page) => page,
        Err(error) => return failure(request, &error),
    };
    if !allows(&request.session_id) {
        return failure(request, SESSION_UNAVAILABLE);
    }
    LocalScrollbackResponse {
        request_id: request.request_id.clone(),
        rows: page.rows,
        cols: u32::from(page.window.cols),
        scrollback_total: u64::from(page.window.total),
        start_row: u64::from(page.window.start_row),
        end_row: u64::from(page.window.end_row),
        grid_epoch: page.window.grid_epoch,
        history_floor: floor_proto(page.floor).into(),
        ..Default::default()
    }
}

struct ReadPage {
    window: Page,
    rows: Vec<PbCellRow>,
    floor: ScrollbackHistoryFloor,
}

/// v2 `readScrollbackCells` with `admitRow`/`continueRead`: a settled core, the
/// epoch fence, the window clamped to the retained floor, and a walk that
/// charges each row against the direct history budget before keeping it.
async fn read_page(
    manager: &SessionManager,
    sessions: &SessionTable,
    request: &LocalScrollbackRequest,
    allows: &(dyn Fn(&str) -> bool + Sync),
) -> Result<ReadPage, String> {
    let session_id = SessionId::try_from(request.session_id.as_str()).map_err(|_| "unknown session".to_owned())?;
    let channel = sessions.channel_of(&session_id).ok_or_else(|| "unknown session".to_owned())?;
    if let Ok(channel_id) = ChannelId::try_from(i64::from(channel)) {
        manager.control_lanes().settled(channel_id).await;
    }
    let description = describe(sessions, &session_id)?;
    let window_request = scrollback_read::Request {
        grid_epoch: request.grid_epoch.clone(),
        end_row: u32::try_from(request.end_row).unwrap_or(u32::MAX),
        max_rows: request.max_rows,
    };
    let window = scrollback_read::page_for(&window_request, description.total, description.cols, &description.binding)
        .map_err(|refusal| refusal_text(refusal).to_owned())?;
    let floor = history_floor_for(&description, window.start_row);
    // v2 clamps the start to the retained floor, so a short page names the
    // surviving suffix and its first absolute row.
    let window = Page { start_row: window.start_row.max(description.retained_floor), ..window };
    let mut rows = Vec::new();
    let mut encoded_bytes = 0usize;
    let mut evicted = false;
    let walk = scrollback_read::walk_page(
        &window,
        |offset| {
            let absolute = window.start_row + offset;
            let Some(row) = sessions.with_record(&session_id, |record| row_cells(record, absolute)).flatten() else {
                evicted = true;
                return false;
            };
            let proto = cell_row_to_proto(&row);
            let row_bytes = proto.encoded_len() as usize + 10;
            if encoded_bytes > ROWS_MAX_BYTES.saturating_sub(row_bytes) {
                return false;
            }
            encoded_bytes += row_bytes;
            rows.push(proto);
            true
        },
        || allows(&request.session_id),
    );
    match walk {
        WalkOutcome::Cancelled { .. } => return Err(SESSION_UNAVAILABLE.to_owned()),
        WalkOutcome::BudgetRefused { .. } if evicted => return Err("scrollback evicted mid-read".to_owned()),
        WalkOutcome::BudgetRefused { .. } => return Err("scrollback response exceeds direct transport limit".to_owned()),
        WalkOutcome::Complete { .. } => {}
    }
    if describe(sessions, &session_id)?.binding.current() != window.grid_epoch {
        return Err("grid epoch changed".to_owned());
    }
    Ok(ReadPage { window, rows, floor })
}

fn describe(sessions: &SessionTable, session_id: &SessionId) -> Result<GridDescription, String> {
    match sessions.with_record(session_id, describe_grid) {
        None => Err("session closed".to_owned()),
        Some(Ok(description)) => Ok(description),
        Some(Err(Refusal::Failed { message, .. })) => Err(message),
        Some(Err(refusal)) => Err(refusal.message()),
    }
}

/// The reader's refusals, worded as v2's reader words them.
fn refusal_text(refusal: scrollback_read::Refusal) -> &'static str {
    match refusal {
        scrollback_read::Refusal::StaleEpoch => "grid epoch changed",
        scrollback_read::Refusal::EmptyRequest => "the request named no rows",
        scrollback_read::Refusal::BudgetRefused => "scrollback response exceeds direct transport limit",
    }
}

/// One total map, so a new floor reason cannot be silently dropped.
fn floor_proto(floor: ScrollbackHistoryFloor) -> PbFloor {
    match floor {
        ScrollbackHistoryFloor::None => PbFloor::Unspecified,
        ScrollbackHistoryFloor::Evicted => PbFloor::Evicted,
        ScrollbackHistoryFloor::ResizeReplay => PbFloor::ResizeReplay,
    }
}

fn failure(request: &LocalScrollbackRequest, error: &str) -> LocalScrollbackResponse {
    LocalScrollbackResponse { request_id: request.request_id.clone(), error: error.to_owned(), ..Default::default() }
}
