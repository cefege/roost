//! What a worker is allowed to have answered to a scrollback search.
//!
//! Ported from `validateSearchRequest` and `parseWorkerSearchResult` in
//! `apps/coord/src/terminal/screen/handlers-sessions-scrollback.ts`. Both are
//! pure, and both exist because the reply is the ONLY thing standing between a
//! buggy or older worker and a browser that pages forever: a result that
//! scanned more rows than were asked for, or that claims a match outside the
//! range it says it scanned, would make the SPA's continuation cursor walk off
//! the end of a grid it cannot see.
//!
//! The rules are therefore stated as disagreements with the REQUEST, not as a
//! schema. A well-formed answer to a different question is still wrong here.

use connectrpc::{ConnectError, ErrorCode};
use roost_protocol::terminal_search::{
    ScrollbackHistoryFloor, TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH, TERMINAL_SEARCH_ID_MAX_LENGTH,
    TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_ROWS, TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS,
    TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
};

use crate::terminal_screen::scrollback_window::{MAX_SAFE_ROW, require_json_safe_row};

/// The refusals every malformed answer carries, so the wording cannot drift
/// between the two methods that produce one.
fn malformed() -> ConnectError {
    ConnectError::new(ErrorCode::Internal, "malformed scrollback search result")
}

fn invalid(message: String) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}

/// A request the worker is allowed to be asked, after the coordinator's checks.
///
/// `before_row` is an optional absolute cursor; it is `None` for "begin at the
/// newest row boundary", which the worker spells as an absent field rather than
/// a zero, so absence is preserved all the way to the frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedSearch {
    pub search_id: String,
    pub grid_epoch: String,
    pub query: String,
    pub before_row: Option<u64>,
    pub max_rows: u32,
    pub max_matches: u32,
}

/// Check one search request, in the order the wire fields are declared.
///
/// The order matters: the cheap shape checks run before the row-index check, so
/// a request that is wrong in three ways is refused for the first one and the
/// message names the field the caller most likely got wrong.
pub fn validate_search_request(
    search_id: &str,
    grid_epoch: &str,
    query: &str,
    max_rows: u32,
    max_matches: u32,
    before_row: Option<u64>,
) -> Result<ValidatedSearch, ConnectError> {
    let search_id_len = search_id.chars().count();
    if !(1..=TERMINAL_SEARCH_ID_MAX_LENGTH).contains(&search_id_len) {
        return Err(invalid(format!(
            "scrollback search search_id must contain 1 to {TERMINAL_SEARCH_ID_MAX_LENGTH} characters"
        )));
    }
    if grid_epoch.chars().count() > TERMINAL_SEARCH_GRID_EPOCH_MAX_LENGTH {
        return Err(invalid(
            "scrollback search grid_epoch is too long".to_owned(),
        ));
    }
    if query.chars().count() > TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS {
        return Err(invalid(format!(
            "scrollback search query must contain at most {TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS} Unicode code points"
        )));
    }
    if !(1..=TERMINAL_SEARCH_MAX_ROWS).contains(&max_rows) {
        return Err(invalid(format!(
            "scrollback search max_rows must be between 1 and {TERMINAL_SEARCH_MAX_ROWS}"
        )));
    }
    if !(1..=TERMINAL_SEARCH_MAX_MATCHES).contains(&max_matches) {
        return Err(invalid(format!(
            "scrollback search max_matches must be between 1 and {TERMINAL_SEARCH_MAX_MATCHES}"
        )));
    }
    if let Some(before_row) = before_row {
        require_json_safe_row(before_row, "scrollback search before_row")?;
    }
    Ok(ValidatedSearch {
        search_id: search_id.to_owned(),
        grid_epoch: grid_epoch.to_owned(),
        query: query.to_owned(),
        before_row,
        max_rows,
        max_matches,
    })
}

/// Why a search stopped, as the wire names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchStop {
    Complete,
    RowLimit,
    MatchLimit,
    Deadline,
    EpochChanged,
}

impl SearchStop {
    /// The wire spelling, from `packages/protocol/src/terminal-search.ts`.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::RowLimit => "row_limit",
            Self::MatchLimit => "match_limit",
            Self::Deadline => "deadline",
            Self::EpochChanged => "epoch_changed",
        }
    }

    /// The validated spelling; an unknown reason is refused rather than mapped
    /// onto "complete", which would tell a browser to stop paging early.
    pub fn parse(value: &str) -> Result<Self, ConnectError> {
        Ok(match value {
            "complete" => Self::Complete,
            "row_limit" => Self::RowLimit,
            "match_limit" => Self::MatchLimit,
            "deadline" => Self::Deadline,
            "epoch_changed" => Self::EpochChanged,
            _ => return Err(malformed()),
        })
    }
}

/// One match, as the worker reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchMatch {
    pub row: u64,
    pub col: u32,
    pub len: u32,
    pub preview: String,
}

/// A whole search page, as the worker reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSearchResult {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    pub scrollback_total: u64,
    pub cols: u32,
    pub grid_epoch: String,
    pub scanned_start_row: u64,
    pub scanned_end_row: u64,
    pub history_floor: ScrollbackHistoryFloor,
    pub next_before_row: Option<u64>,
    pub stop_reason: SearchStop,
}

/// Decode one worker answer, or refuse it as malformed.
pub fn parse_worker_search_result(
    payload: &serde_json::Value,
    request: &ValidatedSearch,
) -> Result<WorkerSearchResult, ConnectError> {
    let row = |field: &str| -> Result<u64, ConnectError> {
        let value = number(payload, field)?;
        if value.fract() != 0.0 || value < 0.0 || value > MAX_SAFE_ROW as f64 {
            return Err(malformed());
        }
        Ok(value as u64)
    };
    let stop_reason = SearchStop::parse(text(payload, "stop_reason")?)?;
    let history_floor =
        ScrollbackHistoryFloor::parse("history_floor", text(payload, "history_floor")?)
            .map_err(|_| malformed())?;
    let matches = parse_matches(payload, request.max_matches as usize)?;
    let grid_epoch = text(payload, "grid_epoch")?.to_owned();
    if grid_epoch.is_empty() {
        return Err(malformed());
    }
    let result = WorkerSearchResult {
        matches,
        truncated: flag(payload, "truncated")?,
        scrollback_total: row("scrollback_total")?,
        cols: positive_u32(payload, "cols")?,
        grid_epoch,
        scanned_start_row: row("scanned_start_row")?,
        scanned_end_row: row("scanned_end_row")?,
        history_floor,
        next_before_row: optional_row(payload, "next_before_row")?,
        stop_reason,
    };
    if disagrees_with_request(&result, request) {
        return Err(malformed());
    }
    Ok(result)
}

fn disagrees_with_request(result: &WorkerSearchResult, request: &ValidatedSearch) -> bool {
    let scanned_rows = result
        .scanned_end_row
        .saturating_sub(result.scanned_start_row);
    // An epoch change and a deadline that scanned nothing are the two answers
    // that legitimately name an epoch the request did not.
    let epoch_mismatch_allowed = result.stop_reason == SearchStop::EpochChanged
        || (result.stop_reason == SearchStop::Deadline
            && result.scanned_start_row == result.scanned_end_row);
    let expected_truncated = matches!(
        result.stop_reason,
        SearchStop::MatchLimit | SearchStop::Deadline
    );
    result.matches.len() > request.max_matches as usize
        || scanned_rows > u64::from(request.max_rows)
        || request.before_row.is_some_and(|before| result.scanned_end_row > before)
        || (result.stop_reason == SearchStop::RowLimit && scanned_rows != u64::from(request.max_rows))
        || (result.stop_reason == SearchStop::MatchLimit
            && result.matches.len() != request.max_matches as usize)
        || (result.truncated != expected_truncated)
        || (!request.grid_epoch.is_empty()
            && result.grid_epoch != request.grid_epoch
            && !epoch_mismatch_allowed)
        || result.scanned_start_row > result.scanned_end_row
        || result
            .matches
            .iter()
            .any(|entry| entry.row < result.scanned_start_row || entry.row >= result.scanned_end_row)
        || (result.stop_reason == SearchStop::RowLimit
            && result.next_before_row != Some(result.scanned_start_row))
        // A match cap leaves older rows unscanned, so it carries the same row
        // cursor a row cap does; navigation stays unbounded through a bounded
        // page.
        || (result.next_before_row.is_some()
            && (!matches!(result.stop_reason, SearchStop::RowLimit | SearchStop::MatchLimit)
                || result.next_before_row != Some(result.scanned_start_row)))
        || (result.stop_reason == SearchStop::Complete
            && result.history_floor == ScrollbackHistoryFloor::None
            && result.scanned_start_row != 0)
}

fn parse_matches(
    payload: &serde_json::Value,
    max_matches: usize,
) -> Result<Vec<SearchMatch>, ConnectError> {
    let Some(raw) = payload.get("matches").and_then(serde_json::Value::as_array) else {
        return Err(malformed());
    };
    if raw.len() > max_matches {
        return Err(malformed());
    }
    raw.iter().map(parse_match).collect()
}

fn parse_match(entry: &serde_json::Value) -> Result<SearchMatch, ConnectError> {
    let preview = text(entry, "preview")?.to_owned();
    if preview.chars().count() > TERMINAL_SEARCH_PREVIEW_MAX_CODE_POINTS {
        return Err(malformed());
    }
    let row = number(entry, "row")?;
    if row.fract() != 0.0 || row < 0.0 || row > MAX_SAFE_ROW as f64 {
        return Err(malformed());
    }
    Ok(SearchMatch {
        row: row as u64,
        col: number(entry, "col")? as u32,
        len: number(entry, "len")? as u32,
        preview,
    })
}

fn text<'a>(value: &'a serde_json::Value, field: &str) -> Result<&'a str, ConnectError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(malformed)
}

fn number(value: &serde_json::Value, field: &str) -> Result<f64, ConnectError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(malformed)
}

fn flag(value: &serde_json::Value, field: &str) -> Result<bool, ConnectError> {
    value
        .get(field)
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(malformed)
}

fn positive_u32(value: &serde_json::Value, field: &str) -> Result<u32, ConnectError> {
    let raw = number(value, field)?;
    if raw < 1.0 || raw > f64::from(u32::MAX) || raw.fract() != 0.0 {
        return Err(malformed());
    }
    Ok(raw as u32)
}

fn optional_row(value: &serde_json::Value, field: &str) -> Result<Option<u64>, ConnectError> {
    let Some(raw) = value.get(field) else {
        return Ok(None);
    };
    if raw.is_null() {
        return Ok(None);
    }
    let row = raw.as_f64().ok_or_else(malformed)?;
    if row.fract() != 0.0 || row < 0.0 || row > MAX_SAFE_ROW as f64 {
        return Err(malformed());
    }
    Ok(Some(row as u64))
}
