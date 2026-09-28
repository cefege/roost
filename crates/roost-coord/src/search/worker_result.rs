//! What a worker may answer to one global-search batch, and what each
//! session's entry means for the page: matches, a typed partial, and where
//! the session resumes.
//!
//! Ports `validateGlobalSearchGroupResult`, `outcomeForGlobalSearchEntry` and
//! their helpers from `apps/coord/src/search/global-search-fanout.ts`, over
//! v2's `WorkerGlobalSearchResultSchema` (`packages/protocol/src/terminal-search.ts`).
//! Each per-session result is decoded by the ONE schema decoder,
//! `terminal_screen::scrollback_result::decode_worker_search_result`. Called by
//! `search::rpc_search`; any lie turns the whole batch into malformed partials.

use std::collections::HashSet;

use roost_proto::GlobalSearchPartialReason;
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS, ScrollbackHistoryFloor,
};
use serde_json::Value;

use crate::search::cursor_types::GlobalSearchSessionPosition;
use crate::search::options::{GlobalSearchPageLimits, allocate_global_search_match_limits};
use crate::terminal_screen::scrollback_result::{
    SearchStop, WorkerSearchResult, decode_worker_search_result,
};

/// Why a worker could not search one session of a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerGlobalSearchError {
    SessionClosed,
    Deadline,
    EpochChanged,
    NoTerminal,
    Internal,
}

impl WorkerGlobalSearchError {
    fn parse(wire: &str) -> Option<Self> {
        Some(match wire {
            "session_closed" => Self::SessionClosed,
            "deadline" => Self::Deadline,
            "epoch_changed" => Self::EpochChanged,
            "no_terminal" => Self::NoTerminal,
            "internal" => Self::Internal,
            _ => return None,
        })
    }
}

/// One session's answer inside a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerGlobalSearchEntry {
    /// The session was scanned.
    Scanned {
        session_id: String,
        result: WorkerSearchResult,
    },
    /// The session could not be scanned.
    Failed {
        session_id: String,
        error: WorkerGlobalSearchError,
    },
}

impl WorkerGlobalSearchEntry {
    /// The session this entry answers for.
    #[must_use]
    pub fn session_id(&self) -> &str {
        match self {
            Self::Scanned { session_id, .. } | Self::Failed { session_id, .. } => session_id,
        }
    }
}

/// One match, projected for the response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchMatch {
    pub session_id: String,
    pub row: u64,
    pub col: u32,
    pub len: u32,
    pub preview: String,
    pub grid_epoch: String,
}

/// What one page did for one session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchSessionOutcome {
    pub matches: Vec<GlobalSearchMatch>,
    pub partial_reason: Option<GlobalSearchPartialReason>,
    pub continuation: Option<GlobalSearchSessionPosition>,
    /// Whether a worker page was consumed for the session.
    pub searched: bool,
}

impl GlobalSearchSessionOutcome {
    /// A session this page never reached: no matches, a partial, and (when
    /// `continuation` is set) a retry of the same position.
    #[must_use]
    pub fn unsearched(
        partial_reason: Option<GlobalSearchPartialReason>,
        continuation: Option<GlobalSearchSessionPosition>,
    ) -> Self {
        Self {
            matches: Vec::new(),
            partial_reason,
            continuation,
            searched: false,
        }
    }
}

/// Decode and check a batch answer against the sessions and budgets that were
/// asked for. `None` is a malformed batch.
#[must_use]
pub fn validate_global_search_group_result(
    raw: &Value,
    sessions: &[GlobalSearchSessionPosition],
    match_budget: u32,
    limits: &GlobalSearchPageLimits,
) -> Option<Vec<WorkerGlobalSearchEntry>> {
    let entries = decode_global_search_result(raw)?;
    if entries.len() != sessions.len() {
        return None;
    }
    let session_budgets = allocate_global_search_match_limits(match_budget, sessions.len());
    let mut match_count = 0_usize;
    let mut identities: HashSet<(&str, &str, u64, u32, u32)> = HashSet::new();
    for ((entry, request), budget) in entries.iter().zip(sessions).zip(&session_budgets) {
        if entry.session_id() != request.session_id {
            return None;
        }
        let WorkerGlobalSearchEntry::Scanned { session_id, result } = entry else {
            continue;
        };
        let scanned_rows = result
            .scanned_end_row
            .saturating_sub(result.scanned_start_row);
        let epoch_mismatch =
            !request.grid_epoch.is_empty() && result.grid_epoch != request.grid_epoch;
        let zero_row_deadline_after_epoch_change =
            epoch_mismatch && result.stop_reason == SearchStop::Deadline && scanned_rows == 0;
        let max_rows = u64::from(limits.max_rows_per_session);
        if (request.grid_epoch.is_empty() && request.before_row.is_some())
            || scanned_rows > max_rows
            || (request.before_row.is_some()
                && !epoch_mismatch
                && Some(result.scanned_end_row) != request.before_row)
            || (result.stop_reason == SearchStop::RowLimit && scanned_rows != max_rows)
            || (epoch_mismatch
                && result.stop_reason != SearchStop::EpochChanged
                && !zero_row_deadline_after_epoch_change)
            || result.matches.len() > *budget as usize
        {
            return None;
        }
        for found in &result.matches {
            let identity = (
                session_id.as_str(),
                result.grid_epoch.as_str(),
                found.row,
                found.col,
                found.len,
            );
            if !identities.insert(identity) {
                return None;
            }
        }
        match_count += result.matches.len();
        if match_count > match_budget as usize {
            return None;
        }
    }
    Some(entries)
}

/// The page outcome of one validated entry.
#[must_use]
pub fn outcome_for_global_search_entry(
    session: &GlobalSearchSessionPosition,
    entry: &WorkerGlobalSearchEntry,
) -> GlobalSearchSessionOutcome {
    let error = match entry {
        WorkerGlobalSearchEntry::Scanned { result, .. } => {
            return outcome_for_search_result(session, result);
        }
        WorkerGlobalSearchEntry::Failed { error, .. } => *error,
    };
    let partial_reason = match error {
        WorkerGlobalSearchError::Deadline => GlobalSearchPartialReason::Deadline,
        WorkerGlobalSearchError::EpochChanged => GlobalSearchPartialReason::EpochChanged,
        WorkerGlobalSearchError::SessionClosed | WorkerGlobalSearchError::NoTerminal => {
            GlobalSearchPartialReason::SessionClosed
        }
        WorkerGlobalSearchError::Internal => GlobalSearchPartialReason::MalformedResult,
    };
    // A malformed entry scanned nothing, so resuming it mid-page would repeat
    // the same request forever. A session with no row position yet has never
    // been searched, and retrying it is real progress.
    let stalled_malformed = partial_reason == GlobalSearchPartialReason::MalformedResult
        && session.before_row.is_some();
    let ends = partial_reason == GlobalSearchPartialReason::SessionClosed || stalled_malformed;
    GlobalSearchSessionOutcome {
        matches: Vec::new(),
        partial_reason: Some(partial_reason),
        continuation: (!ends).then(|| session.clone()),
        searched: true,
    }
}

fn outcome_for_search_result(
    session: &GlobalSearchSessionPosition,
    result: &WorkerSearchResult,
) -> GlobalSearchSessionOutcome {
    let unfloored = result.history_floor == ScrollbackHistoryFloor::None;
    let partial_reason = match result.stop_reason {
        SearchStop::EpochChanged => Some(GlobalSearchPartialReason::EpochChanged),
        SearchStop::MatchLimit => Some(GlobalSearchPartialReason::MatchLimit),
        SearchStop::Deadline => Some(GlobalSearchPartialReason::Deadline),
        _ if !unfloored => Some(GlobalSearchPartialReason::HistoryEvicted),
        _ => None,
    };
    let epoch_mismatch = !session.grid_epoch.is_empty() && session.grid_epoch != result.grid_epoch;
    // A same-epoch deadline page that scanned nothing and resumed exactly
    // where it was asked to start made no progress: handing that position
    // back chains pages forever, each burning a deadline and a worker lane.
    let stalled = !epoch_mismatch
        && result.scanned_end_row == result.scanned_start_row
        && session.before_row == Some(result.scanned_start_row);
    let resume = |grid_epoch: &str, before_row: Option<u64>| GlobalSearchSessionPosition {
        grid_epoch: grid_epoch.to_owned(),
        before_row,
        ..session.clone()
    };
    let continuation = match result.stop_reason {
        SearchStop::EpochChanged => Some(resume("", None)),
        SearchStop::RowLimit | SearchStop::MatchLimit
            if unfloored && result.next_before_row.is_some() =>
        {
            Some(resume(&result.grid_epoch, result.next_before_row))
        }
        SearchStop::Deadline if unfloored && !stalled => Some(if epoch_mismatch {
            resume(&result.grid_epoch, None)
        } else {
            resume(&result.grid_epoch, Some(result.scanned_start_row))
        }),
        _ => None,
    };
    GlobalSearchSessionOutcome {
        matches: result
            .matches
            .iter()
            .map(|found| GlobalSearchMatch {
                session_id: session.session_id.clone(),
                row: found.row,
                col: found.col,
                len: found.len,
                preview: found.preview.clone(),
                grid_epoch: result.grid_epoch.clone(),
            })
            .collect(),
        partial_reason,
        continuation,
        searched: true,
    }
}

/// v2's strict `WorkerGlobalSearchResultSchema`: `{ entries }` only, at most a
/// page of entries, each strict, session ids uuid-shaped and unique.
fn decode_global_search_result(raw: &Value) -> Option<Vec<WorkerGlobalSearchEntry>> {
    let object = raw.as_object()?;
    if object.keys().any(|key| key != "entries") {
        return None;
    }
    let raw_entries = object.get("entries")?.as_array()?;
    if raw_entries.len() > GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS {
        return None;
    }
    let mut seen: HashSet<&str> = HashSet::new();
    let mut entries = Vec::with_capacity(raw_entries.len());
    for raw_entry in raw_entries {
        let entry = decode_entry(raw_entry)?;
        let session_id = raw_entry.get("session_id")?.as_str()?;
        if !seen.insert(session_id) {
            return None;
        }
        entries.push(entry);
    }
    Some(entries)
}

fn decode_entry(raw: &Value) -> Option<WorkerGlobalSearchEntry> {
    let object = raw.as_object()?;
    let session_id = object.get("session_id")?.as_str()?;
    if !is_uuid_shape(session_id) {
        return None;
    }
    let payload_field = match object.get("status")?.as_str()? {
        "ok" => "result",
        "error" => "error",
        _ => return None,
    };
    if object.len() != 3 || !object.contains_key(payload_field) {
        return None;
    }
    let session_id = session_id.to_owned();
    if payload_field == "result" {
        let result = decode_worker_search_result(object.get("result")?).ok()?;
        return Some(WorkerGlobalSearchEntry::Scanned { session_id, result });
    }
    let error = WorkerGlobalSearchError::parse(object.get("error")?.as_str()?)?;
    Some(WorkerGlobalSearchEntry::Failed { session_id, error })
}

/// zod's `.uuid()`: 8-4-4-4-12 hex, any version.
fn is_uuid_shape(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, width)| {
            group.len() == width && group.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}
