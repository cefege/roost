//! The values the global-search cursor owner trades in: a search's identity,
//! its cursor binding, one session's resume position, and the shapes a cursor
//! is issued from and claimed as.
//!
//! Ports the exported types of `apps/coord/src/search/global-search-cursors.ts`.
//! Split from `search::cursors` (the owner) so the owner stays under the file
//! cap; `search::fanout`, `search::worker_result`, `search::cancel` and
//! `search::rpc_search` all name these.

use crate::search::options::GlobalSearchPageLimits;

/// Who a global search belongs to: one device's tab, one search id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlobalSearchIdentity {
    /// The calling account device's fingerprint.
    pub device_fingerprint: String,
    /// The browser tab the request came from (empty for a tab-less cancel).
    pub tab_id: String,
    /// The browser's id for this search.
    pub search_id: String,
}

/// What `begin_search` decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalSearchAdmission {
    /// The search is now active.
    Started,
    /// A live cancel tombstone already retired this identity.
    Cancelled,
    /// The identity already has an active search.
    Duplicate,
    /// The device, or the process, holds its maximum of active searches.
    Capacity,
}

/// Everything a continuation cursor is bound to: a cursor claimed under any
/// other query, case, or page limits is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchCursorBinding {
    /// The search the cursor continues.
    pub identity: GlobalSearchIdentity,
    /// The literal the search looks for.
    pub query: String,
    /// Whether the match is case sensitive.
    pub case_sensitive: bool,
    /// The normalized page limits.
    pub limits: GlobalSearchPageLimits,
}

/// Where one session's next page resumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchSessionPosition {
    /// The session.
    pub session_id: String,
    /// The worker its row named when the page was selected.
    pub worker_fp: String,
    /// The grid epoch a row cursor belongs to; empty before the first page.
    pub grid_epoch: String,
    /// The exclusive row the next page scans below; absent starts at the
    /// newest row.
    pub before_row: Option<u64>,
}

impl GlobalSearchSessionPosition {
    /// A session no page has scanned yet.
    #[must_use]
    pub fn unvisited(session_id: String, worker_fp: String) -> Self {
        Self {
            session_id,
            worker_fp,
            grid_epoch: String::new(),
            before_row: None,
        }
    }
}

/// What a claimed cursor hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchCursorProgress {
    /// The sessions the next page resumes, in cursor order.
    pub sessions: Vec<GlobalSearchSessionPosition>,
    /// The authorized-session denominator the first page counted.
    pub eligible_sessions: usize,
    /// Every session a page of this search has scanned so far.
    pub searched_session_ids: Vec<String>,
}

/// One session's continuation, as a page reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchContinuation {
    /// Where the next page resumes for this session.
    pub position: GlobalSearchSessionPosition,
    /// Whether this page consumed a worker page for the session. A consumed
    /// page MUST advance the row cursor; a session the page never reached
    /// (contended lane, offline worker) legitimately retries its own position.
    pub searched: bool,
    /// The `before_row` this page requested for the session, absent on a
    /// session no page has scanned yet.
    pub requested_before_row: Option<u64>,
}

/// The progress a cursor is issued from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchCursorIssue {
    /// What the cursor is bound to.
    pub binding: GlobalSearchCursorBinding,
    /// The sessions the next page resumes.
    pub continuations: Vec<GlobalSearchContinuation>,
    /// The authorized-session denominator.
    pub eligible_sessions: usize,
    /// Every session scanned so far.
    pub searched_session_ids: Vec<String>,
}

/// Why a cursor was not issued. Each is a coordinator bug, never a caller's:
/// the page that asked for the cursor built its continuations wrongly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CursorIssueRefusal {
    /// No continuation, more than a page, or a denominator smaller than what
    /// it counts.
    UnboundedProgress,
    /// A row cursor without the epoch that gives the row a meaning.
    RowWithoutEpoch,
    /// Two continuations for one session.
    DuplicateSession,
    /// A scanned session handed back at or above the row it was given.
    SearchedSessionDidNotAdvance,
    /// The entropy source for the opaque token failed.
    TokenUnavailable(String),
}

impl std::fmt::Display for CursorIssueRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnboundedProgress => {
                formatter.write_str("global search cursor requires bounded progress")
            }
            Self::RowWithoutEpoch => {
                formatter.write_str("global search row continuation requires a grid epoch")
            }
            Self::DuplicateSession => {
                formatter.write_str("global search continuation sessions must be unique")
            }
            Self::SearchedSessionDidNotAdvance => {
                formatter.write_str("global search continuation must advance a searched session")
            }
            Self::TokenUnavailable(error) => {
                write!(formatter, "global search cursor token unavailable: {error}")
            }
        }
    }
}

/// What `prepare_cancellation` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchCancellationPreparation {
    /// False when a tombstone already exists: a repeated cancel sends nothing.
    pub should_dispatch: bool,
    /// The sessions the active search had selected, for the worker cancels.
    pub selected_sessions: Vec<GlobalSearchSessionPosition>,
}
