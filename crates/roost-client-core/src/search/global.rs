//! The values a fleet-wide terminal-content search answers with.
//!
//! These are the COORDINATOR'S rows, not this client's findings. The query runs
//! against the coordinator's search ledger, which is the only party that knows
//! every machine's retained history; a client-side index would be one machine's
//! view reported as the fleet's, and two search results would then disagree
//! about what exists. Nothing here reads a replica.
//!
//! They live beside `SearchPage` and `FindMatch` because they are the SAME kind
//! of thing — a page of search rows as the coordinator reported it, fenced to
//! the grid epoch that owns each row — and the reconciliation that consumes them
//! stays in `client::global_search`, so the module that HOLDS rows and the module
//! that MERGES them are not the same module.
//!
//! Ported from `apps/web/src/lib/globalContentSearchResults.ts`. Depends on
//! `roost_protocol::wire` for the session brand and adds no state.

use roost_protocol::wire::SessionId;

/// Why one session could not be searched completely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum GlobalSearchPartialReason {
    /// A reason this build does not know.
    Unspecified,
    /// The machine holding the session was not reachable.
    WorkerUnavailable,
    /// The session did not finish inside the page deadline.
    Deadline,
    /// The session's retained history was replaced while it was being read, so
    /// its rows are not comparable with the ones read before it.
    EpochChanged,
    /// The session returned more matches than the page allows.
    MatchLimit,
    /// The session has older history the coordinator no longer retains.
    HistoryEvicted,
    /// The session closed during the search.
    SessionClosed,
    /// The machine returned a result the coordinator refused to believe.
    MalformedResult,
}

impl GlobalSearchPartialReason {
    /// The word a row shows for a partial, and the sentence fragment a host
    /// composes with the session's name.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Unspecified => "returned an unspecified incomplete result",
            Self::WorkerUnavailable => "is unavailable",
            Self::Deadline => "did not finish before the page deadline",
            Self::EpochChanged => "changed while its retained history was searched",
            Self::MatchLimit => "reached its match limit",
            Self::HistoryEvicted => "has older history that is no longer retained",
            Self::SessionClosed => "closed during the search",
            Self::MalformedResult => "returned an invalid search result",
        }
    }

    /// Whether a later successful page can clear this partial.
    ///
    /// Only the three FINAL answers are. `WorkerUnavailable`, `Deadline` and
    /// `MalformedResult` describe a failure of one attempt, so a continuation
    /// that searched that session successfully has replaced them; the other
    /// three describe a property of the session that no continuation changes,
    /// and dropping them would tell a viewer their session was fully searched
    /// when part of it is still missing.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::MatchLimit | Self::HistoryEvicted | Self::SessionClosed)
    }
}

/// One match, fenced to the grid epoch that owns its row.
///
/// The epoch travels WITH the row, exactly as it does for the per-session find
/// results in `crate::search`: a global search spans machines and each machine
/// may resize between pages, so a row whose epoch is looked up later points at
/// whatever that grid has become.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GlobalSearchMatch {
    /// The session the row is in. This is what makes the result fleet-wide: the
    /// row carries the session, and the session carries the machine.
    pub session_id: SessionId,
    /// The row within the session's grid.
    pub row: u64,
    /// The first column of the match.
    pub col: u32,
    /// How many columns the match spans.
    pub len: u32,
    /// The text of the row, for a result list that shows content.
    pub preview: String,
    /// The grid generation this row belongs to.
    pub grid_epoch: String,
}

impl GlobalSearchMatch {
    /// The identity two pages are deduplicated by.
    ///
    /// Session, epoch and cell span — not the preview text, which the machine
    /// may render differently on a later read of the same cell.
    #[must_use]
    pub fn identity(&self) -> String {
        format!(
            "{}\u{0}{}\u{0}{}:{}:{}",
            self.session_id, self.grid_epoch, self.row, self.col, self.len
        )
    }
}

/// One session the search could not finish.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct GlobalSearchPartial {
    /// Which session.
    pub session_id: SessionId,
    /// Why it is partial.
    pub reason: GlobalSearchPartialReason,
}

impl GlobalSearchPartial {
    /// The identity two pages are deduplicated by.
    #[must_use]
    pub fn identity(&self) -> String {
        format!("{}\u{0}{:?}", self.session_id, self.reason)
    }
}
