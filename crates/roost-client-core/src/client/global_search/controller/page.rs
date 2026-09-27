//! Folding one coordinator page into the running search.
//!
//! These are the methods the parent file's state exists to serve, split out only
//! for the line cap. The fence they all ask is the same one: is this page still
//! the page this controller is waiting for?

use std::collections::BTreeSet;

use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
    GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
};
use roost_protocol::wire::SessionId;

use super::{
    GLOBAL_SEARCH_DEBOUNCE_MS, GlobalSearchController, GlobalSearchQuery, GlobalSearchRequest,
    GlobalSearchResults, OutstandingPage, SetSearchOutcome,
};
use crate::client::global_search::reconcile::{
    merge_global_search_matches, reconcile_global_search_partials,
};
use crate::search::global::{GlobalSearchMatch, GlobalSearchPartialReason, GlobalSearchResponse};

impl GlobalSearchController {
    /// Fold one coordinator page in, returning whether it changed what a reader
    /// sees.
    ///
    /// A page answers ONE outstanding call. `call_id` is matched against the
    /// outstanding page, and a `search_id` that is not the running one is
    /// dropped the same way: either names a page for a query the reader has
    /// already moved past, and merging it would append another query's rows to
    /// this one's list and strand the next request on a scan nobody is reading.
    /// The socket reorders, retries and duplicates, and this is the client end
    /// of that.
    ///
    /// A page that is REFUSED returns `false`, and so does one that is
    /// absorbed without a visible change; a page that publishes returns `true`
    /// because publishing is itself the change — it moved the cursor, the
    /// coordinator's own counts and the `has_searched` flag, and cleared a
    /// standing failure.
    ///
    /// No clock argument: nothing a reader sees is stamped, so a deadline read
    /// here would bound a wait nothing consults. The page's own deadline
    /// arrives as a `GlobalSearchPartialReason::Deadline` partial.
    ///
    /// The judging is `reconcile`'s, not this dispatch's: which rows a retired
    /// epoch withdraws, which duplicates a cursor may repeat and which
    /// partials a continuation replaces are all answered there.
    pub fn accept_page(
        &mut self,
        call_id: u64,
        search_id: &str,
        page: &GlobalSearchResponse,
    ) -> bool {
        if !self.is_current(call_id, Some(search_id)) {
            return false;
        }
        // Read BEFORE the fence is cleared: whether this page replaces the
        // published list or appends to it is what the REQUEST asked for, and the
        // answer does not say which it is.
        let appending = self
            .in_flight
            .as_ref()
            .is_some_and(|outstanding| outstanding.cursor.is_some());
        self.in_flight = None;
        let retired: BTreeSet<SessionId> = page
            .partials
            .iter()
            .filter(|partial| partial.reason == GlobalSearchPartialReason::EpochChanged)
            .map(|partial| partial.session_id.clone())
            .collect();
        // A session whose grid was replaced under the scan has no comparable
        // rows at all, in this page or in the ones already published: the epoch
        // they carry is the one that was replaced.
        let incoming: Vec<GlobalSearchMatch> = page
            .matches
            .iter()
            .filter(|candidate| !retired.contains(&candidate.session_id))
            .cloned()
            .collect();
        self.results.matches = if appending {
            let retained: Vec<GlobalSearchMatch> = self
                .results
                .matches
                .iter()
                .filter(|candidate| !retired.contains(&candidate.session_id))
                .cloned()
                .collect();
            merge_global_search_matches(&retained, &incoming)
        } else {
            incoming
        };
        self.results.partials = if appending {
            reconcile_global_search_partials(&self.results.partials, &page.partials)
        } else {
            page.partials.clone()
        };
        self.results.next_cursor = page.next_cursor.clone();
        self.results.searched_sessions = page.searched_sessions;
        self.results.eligible_sessions = page.eligible_sessions;
        self.results.truncated = page.truncated;
        self.results.error = None;
        self.results.retryable = false;
        self.results.has_searched = true;
        true
    }

    /// Fold one failed page in, returning whether it published.
    ///
    /// FENCED ON `call_id` ALONE, and the asymmetry with `accept_page` is the
    /// point rather than an oversight: `effect::RpcResult::Failed` carries a
    /// `call_id` and a message and NOTHING ELSE, so a failure for the previous
    /// attempt cannot be told apart from a failure for this one by any second
    /// key — there is no second key to check. `call_id` is nevertheless
    /// sufficient, because the controller holds exactly ONE outstanding page and
    /// a `call_id` is unique for the life of the client, so a failure for a
    /// superseded page simply does not match the page now outstanding and is
    /// dropped. Checking a key the answer cannot carry would be checking a
    /// value the host had to invent.
    pub fn fail_page(&mut self, call_id: u64, message: String) -> bool {
        if !self.is_current(call_id, None) {
            return false;
        }
        self.in_flight = None;
        self.results.error = Some(message);
        self.results.retryable = true;
        self.results.has_searched = true;
        true
    }

    /// Abandon the running search and return the identity to cancel.
    ///
    /// The published rows are KEPT: an answer the coordinator already gave is
    /// still a true answer, and a viewer who keeps typing should not watch the
    /// list empty on every keystroke.
    pub fn stop_logical_search(&mut self) -> Option<String> {
        self.version = self.version.wrapping_add(1);
        self.debounce_until_ms = None;
        self.in_flight = None;
        self.running = None;
        self.search_id.take()
    }

    /// Abandon everything, for a credential boundary.
    ///
    /// The published rows go too: they were read under a credential that no
    /// longer exists, and a search result is content.
    pub fn reset_for_auth_boundary(&mut self) -> Option<String> {
        let cancel = self.stop_logical_search();
        self.clear_published();
        self.suspended = true;
        cancel
    }

    /// Resume after an auth boundary, restoring exactly what was on screen.
    pub fn resume_after_auth_boundary(&mut self, now_ms: u64) -> SetSearchOutcome {
        if !self.suspended {
            return SetSearchOutcome::Unchanged;
        }
        self.suspended = false;
        let desired = self.desired.clone();
        self.set_search(desired, now_ms)
    }

    pub(super) fn arm(&mut self, query: GlobalSearchQuery, now_ms: u64) {
        self.running = Some(query);
        self.debounce_until_ms = Some(now_ms.saturating_add(GLOBAL_SEARCH_DEBOUNCE_MS));
    }

    pub(super) fn begin_page(
        &mut self,
        query: GlobalSearchQuery,
        call_id: u64,
        cursor: Option<String>,
    ) -> Option<GlobalSearchRequest> {
        let search_id = self.search_id.clone()?;
        let version = self.version;
        self.in_flight = Some(OutstandingPage {
            call_id,
            search_id: search_id.clone(),
            cursor: cursor.clone(),
        });
        Some(GlobalSearchRequest {
            call_id,
            search_id,
            version,
            query: query.query,
            case_sensitive: query.case_sensitive,
            cursor,
            max_sessions: GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS as u32,
            max_rows_per_session: GLOBAL_TERMINAL_SEARCH_ROWS_PER_SESSION,
            max_matches: GLOBAL_TERMINAL_SEARCH_MAX_MATCHES,
        })
    }

    /// A page publishes only if it is the outstanding one AND it belongs to the
    /// running logical search. Both keys, because a retry mints a new
    /// `search_id` under a new `call_id`, and either alone would let the
    /// previous attempt's answer publish into the retried list.
    /// `search_id` is `None` for a failure, which carries no second key; see
    /// [`Self::fail_page`] for why that is a real fence rather than a weaker one.
    fn is_current(&self, call_id: u64, search_id: Option<&str>) -> bool {
        self.in_flight.as_ref().is_some_and(|page| {
            page.call_id == call_id
                && search_id.is_none_or(|search_id| {
                    page.search_id == search_id && self.search_id.as_deref() == Some(search_id)
                })
        })
    }

    pub(super) fn clear_published(&mut self) {
        self.results = GlobalSearchResults::default();
    }
}
