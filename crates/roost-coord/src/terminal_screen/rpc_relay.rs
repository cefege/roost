//! The wire half of the three scrollback RPCs: envelope construction, the
//! wait, the answer's decoding, and the cancel a dropped caller triggers.
//!
//! Split out of `rpc` because the three handlers plus this would sit over the
//! 400-line cap. It is one module rather than four because each piece is only
//! meaningful next to the others: an envelope nobody sends, a decode nobody
//! guards, and a cancel nobody arms are three ways to write a relay that
//! forwards and forgets.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::{
    PbCellRow, ScrollbackHistoryFloor as PbHistoryFloor, SearchStopReason as PbStopReason,
    SessionsGetScrollbackCellsResponse, SessionsSearchScrollbackResponse,
};
use roost_protocol::terminal_search::ScrollbackHistoryFloor;
use roost_protocol::wire::SessionId;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::control::global_search::TerminalSearchId;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde_json::Value;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::terminal_screen::pending_rpcs::PendingRpc;
use crate::terminal_screen::scrollback_relay::{ScrollbackRelay, SessionWorkerBinding};
use crate::terminal_screen::scrollback_result::{SearchStop, WorkerSearchResult};
use crate::terminal_screen::scrollback_window::{
    ScrollbackRowOrder, ScrollbackWindow, check_row_order,
};

/// A fresh correlation id for one browser-to-worker request.
///
/// v2 mints a v4 uuid per request. This is a monotonic counter: the id only has
/// to be unique among the entries in one correlation table, and that table
/// namespaces it by worker anyway, so a random source would be buying a
/// property nothing checks.
#[must_use]
pub fn request_id() -> String {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    format!("rpc-{}", SEQUENCE.fetch_add(1, Ordering::Relaxed))
}

/// The branded session id a request names, or the refusal a malformed one earns.
pub fn session_id(raw: &str) -> Result<SessionId, ConnectError> {
    SessionId::try_from(raw)
        .map_err(|error| ConnectError::new(ErrorCode::InvalidArgument, error.to_string()))
}

/// Write one downstream frame through a handle.
///
/// A refused send is a `Unavailable`, never a panic: the caller's pending
/// correlation entry is settled from the same path, and a panic there would
/// take down the connection every other in-flight search on it shares.
pub fn write(handle: &WorkerHandle, downstream: CoordWorkerDownstream) -> Result<(), ConnectError> {
    if handle.is_revoked() {
        return Err(ConnectError::new(
            ErrorCode::Unavailable,
            "send failed: worker generation is fenced",
        ));
    }
    handle.send(downstream);
    Ok(())
}

/// Wrap one control frame in the downstream envelope and write it.
///
/// `browser_id` and `viewer_id` are opaque to the worker -- it does not learn
/// who is watching -- and a search carries the tab-scoped viewer id in both,
/// which is what makes a cancel from one tab unable to stop another tab's scan.
pub fn send_browser_command(
    handle: &WorkerHandle,
    viewer_id: &str,
    request_id: &str,
    frame: ClientControlFrame,
) -> Result<(), ConnectError> {
    write(
        handle,
        CoordWorkerDownstream::BrowserCommand {
            browser_id: viewer_id.to_owned(),
            viewer_id: viewer_id.to_owned(),
            request_id: request_id.to_owned(),
            frame,
            trace_id: None,
        },
    )
}

/// Wait for a history page, mapping the deadline to the wording that method
/// uses rather than the search's.
pub async fn await_page(pending: &mut PendingRpc, deadline_ms: u64) -> Result<Value, ConnectError> {
    let timed_out =
        || ConnectError::new(ErrorCode::Unavailable, "scrollback cells serve timed out");
    match tokio::time::timeout(Duration::from_millis(deadline_ms), pending.settle()).await {
        Ok(Ok(payload)) => Ok(payload),
        Ok(Err(error)) => Err(match error.code {
            ErrorCode::DeadlineExceeded => timed_out(),
            _ => ConnectError::new(
                ErrorCode::Internal,
                format!("scrollback cells serve failed: {}", error_text(&error)),
            ),
        }),
        Err(_) => Err(timed_out()),
    }
}

/// Wait for a search page.
///
/// `invalid regex: ...` is the one worker error that is the CALLER's fault: the
/// query reached the worker and the worker's own regex engine refused it, so it
/// is `InvalidArgument` here rather than a coordinator failure.
pub async fn await_search(
    pending: &mut PendingRpc,
    deadline_ms: u64,
) -> Result<Value, ConnectError> {
    let timed_out = || ConnectError::new(ErrorCode::Unavailable, "scrollback search timed out");
    match tokio::time::timeout(Duration::from_millis(deadline_ms), pending.settle()).await {
        Ok(Ok(payload)) => Ok(payload),
        Ok(Err(error)) => Err(match error.code {
            ErrorCode::InvalidArgument => {
                ConnectError::new(ErrorCode::InvalidArgument, error_text(&error))
            }
            ErrorCode::DeadlineExceeded => timed_out(),
            _ if error_text(&error).starts_with("invalid regex: ") => {
                ConnectError::new(ErrorCode::InvalidArgument, error_text(&error))
            }
            _ => error,
        }),
        Err(_) => Err(timed_out()),
    }
}

/// Decode one history page and check it against the window the request named.
pub fn decode_cells_page(
    payload: &Value,
    window: &ScrollbackWindow,
) -> Result<SessionsGetScrollbackCellsResponse, ConnectError> {
    let rows_json = payload
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(malformed_page)?;
    let start_row = json_row(payload, "start_row")?;
    let end_row = json_row(payload, "end_row")?;
    let cols = json_positive_u32(payload, "cols")?;
    let total = json_row(payload, "total")?;
    let grid_epoch = payload
        .get("grid_epoch")
        .and_then(Value::as_str)
        .ok_or_else(malformed_page)?
        .to_owned();
    // A worker older than the field reports nothing, which is exactly
    // UNSPECIFIED: no floor claim, rather than a guessed one.
    let history_floor = match payload.get("history_floor").and_then(Value::as_str) {
        Some(raw) => ScrollbackHistoryFloor::parse("history_floor", raw)
            .unwrap_or(ScrollbackHistoryFloor::None),
        None => ScrollbackHistoryFloor::None,
    };
    let mut rows: Vec<PbCellRow> = Vec::with_capacity(rows_json.len());
    for row in rows_json {
        rows.push(serde_json::from_value(row.clone()).map_err(|_| malformed_page())?);
    }
    let indices: Vec<u32> = rows.iter().map(|row| row.index).collect();
    if let ScrollbackRowOrder::Refused(reason) = check_row_order(start_row, &indices) {
        return Err(ConnectError::new(ErrorCode::Internal, reason));
    }
    if !window.is_served_by(start_row, end_row) {
        return Err(ConnectError::new(
            ErrorCode::Internal,
            "scrollback cells page does not cover the requested window",
        ));
    }
    Ok(SessionsGetScrollbackCellsResponse {
        rows,
        cols,
        scrollback_total: total,
        start_row,
        end_row,
        grid_epoch,
        history_floor: history_floor_to_proto(&history_floor).into(),
        __buffa_unknown_fields: Default::default(),
    })
}

/// Encode one validated search page.
#[must_use]
pub fn encode_search_response(result: WorkerSearchResult) -> SessionsSearchScrollbackResponse {
    SessionsSearchScrollbackResponse {
        matches: result
            .matches
            .into_iter()
            .map(|entry| roost_proto::SessionsSearchScrollbackMatch {
                row: entry.row,
                col: entry.col,
                len: entry.len,
                preview: entry.preview,
                __buffa_unknown_fields: Default::default(),
            })
            .collect(),
        truncated: result.truncated,
        scrollback_total: result.scrollback_total,
        cols: result.cols,
        grid_epoch: result.grid_epoch,
        scanned_start_row: result.scanned_start_row,
        scanned_end_row: result.scanned_end_row,
        history_floor: history_floor_to_proto(&result.history_floor).into(),
        next_before_row: result.next_before_row,
        stop_reason: stop_reason_to_proto(result.stop_reason).into(),
        __buffa_unknown_fields: Default::default(),
    }
}

/// The floor, in the proto's vocabulary. One total map, so a floor a newer peer
/// names degrades to "no claim" rather than falling off a `match` with no arm.
#[must_use]
pub fn history_floor_to_proto(floor: &ScrollbackHistoryFloor) -> PbHistoryFloor {
    match floor {
        ScrollbackHistoryFloor::None => PbHistoryFloor::Unspecified,
        ScrollbackHistoryFloor::Evicted => PbHistoryFloor::Evicted,
        ScrollbackHistoryFloor::ResizeReplay => PbHistoryFloor::ResizeReplay,
        ScrollbackHistoryFloor::Other(_) => PbHistoryFloor::Unspecified,
    }
}

/// The stop reason, in the proto's vocabulary.
#[must_use]
pub fn stop_reason_to_proto(stop: SearchStop) -> PbStopReason {
    match stop {
        SearchStop::Complete => PbStopReason::Complete,
        SearchStop::RowLimit => PbStopReason::RowLimit,
        SearchStop::MatchLimit => PbStopReason::MatchLimit,
        SearchStop::Deadline => PbStopReason::Deadline,
        SearchStop::EpochChanged => PbStopReason::EpochChanged,
    }
}

/// Tells the worker to stop a search whose caller went away.
///
/// Armed only after the search frame actually went out, because a cancel for a
/// command that was never sent names a scan that does not exist. Dropping this
/// is the abort: a cancelled HTTP request, a dropped Connect stream, and a
/// handler future torn down mid-await all land here.
#[derive(Debug)]
pub struct CancelSearchOnDrop {
    relay: ScrollbackRelay,
    binding: SessionWorkerBinding,
    session_id: SessionId,
    search_id: String,
    viewer_id: String,
    armed: bool,
}

impl CancelSearchOnDrop {
    /// A guard that has not sent anything yet, and so has nothing to cancel.
    #[must_use]
    pub fn new(
        relay: ScrollbackRelay,
        binding: SessionWorkerBinding,
        session_id: SessionId,
        search_id: String,
        viewer_id: String,
    ) -> Self {
        Self {
            relay,
            binding,
            session_id,
            search_id,
            viewer_id,
            armed: false,
        }
    }

    /// Mark the search as forwarded, so a drop from here on tells the worker.
    pub fn arm(&mut self) {
        self.armed = true;
    }
}

impl Drop for CancelSearchOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let identity =
            self.relay
                .search_identity(&self.viewer_id, &self.session_id, &self.search_id);
        self.relay.record_cancel(&identity);
        let Ok(search_request_id) = TerminalSearchId::try_from(self.search_id.as_str()) else {
            return;
        };
        let downstream = CoordWorkerDownstream::BrowserCommand {
            browser_id: self.viewer_id.clone(),
            viewer_id: self.viewer_id.clone(),
            request_id: self.search_id.clone(),
            frame: ClientControlFrame::CancelScrollbackSearch {
                request_id: self.search_id.clone(),
                session_id: self.session_id.clone(),
                search_request_id,
                trace_id: None,
            },
            trace_id: None,
        };
        if write(&self.binding.handle, downstream).is_err() {
            tracing::warn!(
                session_id = %self.session_id,
                search_id = %self.search_id,
                "a dropped scrollback search could not be cancelled on the worker"
            );
        }
    }
}

/// The message a `ConnectError` carries, or the empty string when it has none.
#[must_use]
pub fn error_text(error: &ConnectError) -> &str {
    error.message.as_deref().unwrap_or_default()
}

fn malformed_page() -> ConnectError {
    ConnectError::new(ErrorCode::Internal, "malformed scrollback cells result")
}

fn json_row(value: &Value, field: &str) -> Result<u64, ConnectError> {
    let row = value
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(malformed_page)?;
    if row.fract() != 0.0 || row < 0.0 {
        return Err(malformed_page());
    }
    crate::terminal_screen::scrollback_window::require_json_safe_row(row as u64, field)?;
    Ok(row as u64)
}

fn json_positive_u32(value: &Value, field: &str) -> Result<u32, ConnectError> {
    let raw = value
        .get(field)
        .and_then(Value::as_f64)
        .ok_or_else(malformed_page)?;
    if raw < 1.0 || raw > f64::from(u32::MAX) || raw.fract() != 0.0 {
        return Err(malformed_page());
    }
    Ok(raw as u32)
}
