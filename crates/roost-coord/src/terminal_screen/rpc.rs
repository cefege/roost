//! `SessionsGetScrollbackCells`, `SessionsSearchScrollback` and
//! `SessionsCancelScrollbackSearch`.
//!
//! Ported from `apps/coord/src/terminal/screen/handlers-sessions-scrollback.ts`.
//! Searches and history pages run on the WORKER, because the SPA holds only a
//! bounded window of the authoritative grid; this module owns the request's
//! shape, the correlation, the refusal wording, and the answer's validation.
//! The wire half lives in `rpc_relay`.
//!
//! The Connect method names this file answers, and the function that answers
//! each, for the single `service_impl.rs` pass. Nothing here is wired into that
//! block: the impl is one file every domain also has to edit (contract §12.2).

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use roost_proto::{
    SessionsCancelScrollbackSearchRequest, SessionsCancelScrollbackSearchResponse,
    SessionsGetScrollbackCellsRequest, SessionsGetScrollbackCellsResponse,
    SessionsSearchScrollbackRequest, SessionsSearchScrollbackResponse,
};
use roost_protocol::terminal_search::{
    TERMINAL_SEARCH_ID_MAX_LENGTH, TERMINAL_SEARCH_RPC_DEADLINE_MS,
};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::control::global_search::{
    TerminalSearchGridEpoch, TerminalSearchId, TerminalSearchQuery, TerminalSearchRow,
};

use crate::coord_core::{Caller, CoordCore};
use crate::terminal_screen::rpc_relay::{
    CancelSearchOnDrop, await_page, await_search, decode_cells_page, encode_search_response,
    error_text, request_id, send_browser_command, session_id,
};
use crate::terminal_screen::scrollback_relay::ScrollbackRelay;
use crate::terminal_screen::scrollback_result::{
    parse_worker_search_result, validate_search_request,
};
use crate::terminal_screen::scrollback_window::ScrollbackWindow;

/// The deadline a history PAGE is served under.
///
/// v2 passes 8000 here rather than `TERMINAL_SEARCH_RPC_DEADLINE_MS`. The two
/// are the same number today and are named separately because they answer to
/// different budgets: conflating them is how a page deadline ends up owned by
/// search policy, and the page is the path a scrollbar drags on.
const CELLS_RPC_DEADLINE_MS: u64 = 8_000;

/// Every Connect method this file answers, and the function that answers it.
pub const SCROLLBACK_METHODS: &[(&str, &str)] = &[
    (
        "SessionsGetScrollbackCells",
        "handle_sessions_get_scrollback_cells",
    ),
    (
        "SessionsSearchScrollback",
        "handle_sessions_search_scrollback",
    ),
    (
        "SessionsCancelScrollbackSearch",
        "handle_sessions_cancel_scrollback_search",
    ),
];

/// Serve one window of a session's scrollback, oldest row first.
pub async fn handle_sessions_get_scrollback_cells(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsGetScrollbackCellsRequest,
) -> ServiceResult<SessionsGetScrollbackCellsResponse> {
    let relay = relay(core)?;
    require_account_device(caller)?;
    let session = session_id(&req.session_id)?;
    let window =
        ScrollbackWindow::for_request(req.end_row, req.max_rows, "scrollback cells end_row")?;
    let binding = relay
        .session_worker_socket(&core.services.db, &session)
        .await?;
    let mut pending = relay.pending().create(
        &request_id(),
        Some(binding.worker_fp.as_str()),
        relay.now_ms(),
    )?;
    let send = send_browser_command(
        &binding.handle,
        caller.fingerprint(),
        pending.request_id(),
        ClientControlFrame::GetScrollbackCells {
            request_id: pending.request_id().to_owned(),
            session_id: session,
            grid_epoch: req.grid_epoch,
            end_row: i64::try_from(window.end_row).unwrap_or(i64::MAX),
            max_rows: i64::from(req.max_rows),
            trace_id: None,
        },
    );
    if let Err(error) = send {
        relay.pending().reject_unavailable(
            pending.request_id(),
            error_text(&error),
            Some(binding.worker_fp.as_str()),
        );
        return Err(error);
    }
    let payload = await_page(&mut pending, CELLS_RPC_DEADLINE_MS).await?;
    Response::ok(decode_cells_page(&payload, &window)?)
}

/// Scan one bounded page of a session's scrollback on its worker.
pub async fn handle_sessions_search_scrollback(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsSearchScrollbackRequest,
) -> ServiceResult<SessionsSearchScrollbackResponse> {
    let relay = relay(core)?;
    require_account_device(caller)?;
    let viewer_id = ScrollbackRelay::viewer_id(caller.fingerprint(), caller.tab_id.as_deref())?;
    let session = session_id(&req.session_id)?;
    let validated = validate_search_request(
        &req.search_id,
        &req.grid_epoch,
        &req.query,
        req.max_rows,
        req.max_matches,
        req.before_row,
    )?;
    let identity = relay.search_identity(&viewer_id, &session, &validated.search_id);
    // A cancel that already beat this search retires it here, before any frame
    // is sent and before a correlation entry exists. Finding the tombstone IS
    // the retirement, so a later search under the same id starts clean.
    if relay.consume_cancel(&identity) {
        return Err(ConnectError::new(
            ErrorCode::Canceled,
            "browser request cancelled",
        ));
    }
    let binding = relay
        .session_worker_socket(&core.services.db, &session)
        .await?;
    let before_row = validated
        .before_row
        .map(|row| i64::try_from(row).map_err(|_| row_index_fault()))
        .transpose()?
        .map(|row| {
            TerminalSearchRow::try_from(row)
                .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))
        })
        .transpose()?;
    let mut pending = relay.pending().create(
        &request_id(),
        Some(binding.worker_fp.as_str()),
        relay.now_ms(),
    )?;
    let frame = ClientControlFrame::SearchScrollback {
        request_id: pending.request_id().to_owned(),
        session_id: session.clone(),
        search_id: branded_id(&validated.search_id)?,
        grid_epoch: TerminalSearchGridEpoch::try_from(validated.grid_epoch.as_str())
            .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))?,
        query: TerminalSearchQuery::try_from(validated.query.as_str())
            .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))?,
        case_sensitive: req.case_sensitive,
        regex: req.regex,
        before_row,
        max_rows: i64::from(validated.max_rows),
        max_matches: i64::from(validated.max_matches),
        trace_id: None,
    };
    let mut cancel_on_drop = CancelSearchOnDrop::new(
        relay.clone(),
        binding.clone(),
        session,
        validated.search_id.clone(),
        viewer_id.clone(),
    );
    if let Err(error) =
        send_browser_command(&binding.handle, &viewer_id, pending.request_id(), frame)
    {
        relay.pending().reject_unavailable(
            pending.request_id(),
            error_text(&error),
            Some(binding.worker_fp.as_str()),
        );
        return Err(error);
    }
    cancel_on_drop.arm();
    let payload = await_search(&mut pending, u64::from(TERMINAL_SEARCH_RPC_DEADLINE_MS)).await?;
    let result = parse_worker_search_result(&payload, &validated)?;
    Response::ok(encode_search_response(result))
}

/// Call a running scrollback search off, by the identity its tab owns.
pub async fn handle_sessions_cancel_scrollback_search(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsCancelScrollbackSearchRequest,
) -> ServiceResult<SessionsCancelScrollbackSearchResponse> {
    let relay = relay(core)?;
    require_account_device(caller)?;
    let search_id_len = req.search_id.chars().count();
    if search_id_len < 1 || search_id_len > TERMINAL_SEARCH_ID_MAX_LENGTH {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            format!(
                "scrollback search search_id must contain 1 to {TERMINAL_SEARCH_ID_MAX_LENGTH} characters"
            ),
        ));
    }
    let session = session_id(&req.session_id)?;
    let viewer_id =
        ScrollbackRelay::cancel_viewer_id(caller.fingerprint(), caller.tab_id.as_deref());
    // Recorded BEFORE the worker is reached, so a cancel that fails to send
    // still retires a search that has not been forwarded yet -- which is the
    // ordering that v2 has no way to express.
    let identity = relay.search_identity(&viewer_id, &session, &req.search_id);
    relay.record_cancel(&identity);
    let binding = relay
        .session_worker_socket(&core.services.db, &session)
        .await?;
    send_browser_command(
        &binding.handle,
        &viewer_id,
        &req.search_id,
        ClientControlFrame::CancelScrollbackSearch {
            request_id: req.search_id.clone(),
            session_id: session,
            search_request_id: branded_id(&req.search_id)?,
            trace_id: None,
        },
    )?;
    Response::ok(SessionsCancelScrollbackSearchResponse::default())
}

/// The relay this handler reads its process state from.
fn relay(core: &CoordCore) -> Result<&ScrollbackRelay, ConnectError> {
    Ok(&core.services.scrollback)
}

/// Every method here reads a session's history, so every method here needs an
/// account device rather than a worker key or a legacy one.
fn require_account_device(caller: &Caller) -> Result<(), ConnectError> {
    caller
        .principal
        .require_account_device()
        .map(|_| ())
        .map_err(|error| ConnectError::new(ErrorCode::PermissionDenied, error.to_string()))
}

fn branded_id(search_id: &str) -> Result<TerminalSearchId, ConnectError> {
    TerminalSearchId::try_from(search_id)
        .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))
}

fn row_index_fault() -> ConnectError {
    ConnectError::new(
        ErrorCode::InvalidArgument,
        "scrollback search before_row is not addressable on this worker",
    )
}
