//! The fleet-wide terminal-content search, as a state machine over coordinator
//! pages: a debounced first page, a cursor continuation, and a cancel owed to
//! the coordinator when either is abandoned.
//!
//! THERE IS NO CLIENT-SIDE INDEX AND THERE MUST NOT BE ONE. Every row published
//! here is a row the coordinator read out of ITS search ledger, which is the
//! only party that knows every machine's retained history. A client that
//! indexed the machines it happens to be connected to would answer "no results"
//! for a session on a machine it has not dialed and would report that as the
//! fleet's answer. The request below therefore carries no machine field at all,
//! and the caps it sends are the coordinator's own.
//!
//! Two fences, and they are the same two the rest of the core uses: a VERSION
//! that a superseded request can never publish through, and a `search_id` the
//! coordinator can cancel. A page whose version is not the current one is
//! dropped whole, because merging it would append another query's rows to this
//! one's list.
//!
//! Ported from `apps/web/src/lib/globalContentSearchController.ts`. The limits
//! are `roost_protocol::terminal_search` and are not restated here.

// The page-folding half of this type lives in `controller/page.rs`, a CHILD
// module: an inherent `impl` may span files, and a child is also the only
// place that may read this file's private fence fields without loosening them
// for the whole crate. The module is `controller`, not `global_search`, which
// named itself — `clippy::module_inception`.
mod page;
use roost_protocol::terminal_search::{
    TERMINAL_SEARCH_ID_MAX_LENGTH, TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
};

use crate::search::global::{GlobalSearchMatch, GlobalSearchPartial};

/// How long typing settles before a query is sent.
///
/// Debounced because a query is a fleet-wide scan: one request per keystroke
/// turns a five-letter word into five scans of every retained session on every
/// machine, and an abandoned scan is work the coordinator cannot un-do.
pub const GLOBAL_SEARCH_DEBOUNCE_MS: u64 = 300;

/// What a viewer is asking for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalSearchQuery {
    /// The text to find. Whitespace-only is the same as empty.
    pub query: String,
    /// Whether the match respects case.
    pub case_sensitive: bool,
}

impl GlobalSearchQuery {
    /// Whether this query would actually search anything.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.query.trim().is_empty()
    }

    /// How many code points the query is, which is what the shared limit counts.
    #[must_use]
    pub fn code_points(&self) -> usize {
        self.query.chars().count()
    }
}

/// What `set_search` decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetSearchOutcome {
    /// The same query is already running, or an auth boundary has suspended
    /// the search; nothing was touched.
    Unchanged,
    /// The query is empty, so the published state was cleared.
    Cleared,
    /// The query is longer than the shared limit, so nothing was sent.
    TooLong,
    /// A new logical search was armed and will be sent when its debounce ends.
    Debouncing,
}

/// One page request, ready for a host to issue as a Connect call.
///
/// `call_id` is the caller's, so a host correlates the answer with the same
/// counter every other call in this crate uses. `version` is the fence: it is
/// copied into every page this logical search issues, and checked on arrival.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSearchRequest {
    /// Correlates the answer with this call.
    pub call_id: u64,
    /// The identity the coordinator cancels this search under.
    pub search_id: String,
    /// The fence for this logical search.
    pub version: u64,
    /// The text to find.
    pub query: String,
    /// Whether the match respects case.
    pub case_sensitive: bool,
    /// Where to resume, or `None` for the first page.
    pub cursor: Option<String>,
    /// The coordinator's own session cap, never this client's.
    pub max_sessions: u32,
    /// The coordinator's own per-session row cap.
    pub max_rows_per_session: u32,
    /// The coordinator's own match cap.
    pub max_matches: u32,
}

/// What the search has published.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalSearchResults {
    /// The rows read so far, across every machine the coordinator searched.
    pub matches: Vec<GlobalSearchMatch>,
    /// The sessions that could not be finished, and why.
    pub partials: Vec<GlobalSearchPartial>,
    /// Where the next page resumes.
    pub next_cursor: Option<String>,
    /// How many sessions the coordinator has actually searched.
    pub searched_sessions: u32,
    /// How many were eligible, as the COORDINATOR counted them.
    pub eligible_sessions: u32,
    /// Whether the coordinator stopped for a cap.
    pub truncated: bool,
    /// Whether a search has been sent and answered at all, as against an empty
    /// box that simply has not been used.
    pub has_searched: bool,
    /// A reader-facing failure, or `None` while the last answer stands.
    pub error: Option<String>,
    /// Whether the failure is worth offering a retry for.
    pub retryable: bool,
}

/// The one page this controller is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutstandingPage {
    /// The call this page answers.
    pub call_id: u64,
    /// The identity the coordinator knows it under.
    pub search_id: String,
    /// Where this page resumes, or `None` for a first page.
    ///
    /// Kept because it decides whether an answer REPLACES the published list or
    /// appends to it, and the answer does not say which it is.
    pub cursor: Option<String>,
}

/// The fleet-wide search, as state.
#[derive(Debug, Default)]
pub struct GlobalSearchController {
    /// What the viewer last asked for, whether or not it is running. Kept so an
    /// auth boundary can restore exactly what was on screen.
    desired: GlobalSearchQuery,
    /// The running query, or `None` when nothing is running.
    running: Option<GlobalSearchQuery>,
    /// The identity the coordinator can cancel the running search under.
    search_id: Option<String>,
    /// Bumped on every logical change. A page issued under an older one can
    /// never publish.
    version: u64,
    /// The page this controller is waiting for, and nothing else.
    ///
    /// The ANSWER carries `call_id` and `search_id` and nothing more
    /// (`effect::RpcResult::SearchPage`), so the fence is keyed on exactly
    /// those: a handler that had to reconstruct the version, the query and the
    /// cursor to check a late page would be reconstructing the request it
    /// already has, and the copy it reconstructs from is the thing that can
    /// drift.
    in_flight: Option<OutstandingPage>,
    /// When the debounced first page may be sent, or `None` when none is due.
    debounce_until_ms: Option<u64>,
    /// The published state.
    results: GlobalSearchResults,
    /// Set while an auth boundary has suspended the search.
    suspended: bool,
}

impl GlobalSearchController {
    /// A controller that has never searched.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The published state, for a host that renders it.
    #[must_use]
    pub fn results(&self) -> &GlobalSearchResults {
        &self.results
    }

    /// The query the viewer last asked for.
    #[must_use]
    pub fn desired(&self) -> &GlobalSearchQuery {
        &self.desired
    }

    /// The identity to cancel at the coordinator, or `None` when nothing is
    /// running.
    ///
    /// A host that abandons a search — because the route changed, or because a
    /// credential was cut — must send the cancel. The coordinator's own ledger
    /// keeps a scan running for a cursor's whole lifetime otherwise, and that
    /// scan is work no viewer is waiting for.
    #[must_use]
    pub fn active_search_id(&self) -> Option<&str> {
        self.search_id.as_deref()
    }

    /// Whether a page is outstanding.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The page this controller is waiting for, for a host that routes an
    /// answer back to the search that asked for it.
    #[must_use]
    pub fn outstanding_page(&self) -> Option<&OutstandingPage> {
        self.in_flight.as_ref()
    }

    /// Whether a debounced first page is waiting to be sent.
    #[must_use]
    pub fn is_debouncing(&self) -> bool {
        self.debounce_until_ms.is_some()
    }

    /// Whether an auth boundary has suspended the search.
    #[must_use]
    pub fn is_suspended(&self) -> bool {
        self.suspended
    }

    /// Ask for a different query.
    pub fn set_search(&mut self, query: GlobalSearchQuery, now_ms: u64) -> SetSearchOutcome {
        self.desired = query.clone();
        if self.suspended {
            return SetSearchOutcome::Unchanged;
        }
        if self.running.as_ref() == Some(&query) && self.results.error.is_none() {
            return SetSearchOutcome::Unchanged;
        }
        self.stop_logical_search();
        self.clear_published();
        if query.is_empty() {
            return SetSearchOutcome::Cleared;
        }
        if query.code_points() > TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS {
            self.results.error = Some(format!(
                "Terminal content queries are limited to \
                 {TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS} characters."
            ));
            self.results.has_searched = true;
            return SetSearchOutcome::TooLong;
        }
        self.arm(query, now_ms);
        SetSearchOutcome::Debouncing
    }

    /// Send the running query again from its first page, after a failure.
    ///
    /// Re-arms the same debounce a fresh keystroke would: a retry is still a
    /// fleet-wide scan, and one that skipped the settle would be the third scan
    /// this query caused in a second.
    pub fn retry(&mut self, now_ms: u64) -> SetSearchOutcome {
        let Some(query) = self.running.clone() else {
            return SetSearchOutcome::Unchanged;
        };
        self.set_search(query, now_ms)
    }

    /// Take the first page request, if the debounce has ended and nothing is
    /// already outstanding.
    ///
    /// One page in flight at a time: the coordinator's cursor is a single
    /// continuation, and a second concurrent page would be a second reader of
    /// one scan. The host mints `search_id` — randomness is a platform concern
    /// — and this refuses one too long to be a bounded coordinator key.
    pub fn take_first_page(
        &mut self,
        search_id: &str,
        call_id: u64,
        now_ms: u64,
    ) -> Option<GlobalSearchRequest> {
        let due = self.debounce_until_ms?;
        if now_ms < due || self.in_flight.is_some() {
            return None;
        }
        if search_id.is_empty() || search_id.len() > TERMINAL_SEARCH_ID_MAX_LENGTH {
            self.debounce_until_ms = None;
            self.results.error = Some("Unable to create a bounded search identifier.".to_owned());
            self.results.has_searched = true;
            return None;
        }
        let query = self.running.clone()?;
        self.debounce_until_ms = None;
        self.search_id = Some(search_id.to_owned());
        self.begin_page(query, call_id, None)
    }

    /// Take the next page request, if there is a cursor and nothing is
    /// outstanding.
    pub fn load_more(&mut self, call_id: u64) -> Option<GlobalSearchRequest> {
        if self.in_flight.is_some() || self.debounce_until_ms.is_some() {
            return None;
        }
        let cursor = self.results.next_cursor.clone()?;
        let query = self.running.clone()?;
        self.begin_page(query, call_id, Some(cursor))
    }
}
