//! `SessionsSearchGlobal`: coordinator-authorized, install-wide terminal
//! content search.
//!
//! Ports `apps/coord/src/search/handlers-sessions-global-search.ts`. It
//! enumerates live session authority from SQLite, fans one bounded batch to
//! each routable worker (`search::page_group`), and projects matches,
//! partials, and a continuation cursor. Continuation and cancel ordering are
//! `services.search.cursors()`'s. Called from the `SessionsSearchGlobal` arm in
//! `rpc/service_impl.rs`; a dropped call is v2's aborted request.

use std::collections::{HashMap, HashSet};

use connectrpc::{ConnectError, ErrorCode, Response, ServiceResult};
use futures_util::future::join_all;
use roost_proto::{
    GlobalSearchPartialReason, SessionsSearchGlobalMatch, SessionsSearchGlobalPartial,
    SessionsSearchGlobalRequest, SessionsSearchGlobalResponse,
};
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS, TERMINAL_SEARCH_ID_MAX_LENGTH,
    TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS,
};

use crate::auth::principal::require_account_device;
use crate::coord_core::{Caller, CoordCore};
use crate::search::cursor_types::{
    CursorIssueRefusal, GlobalSearchAdmission, GlobalSearchContinuation, GlobalSearchCursorBinding,
    GlobalSearchCursorIssue, GlobalSearchIdentity, GlobalSearchSessionPosition,
};
use crate::search::fanout::{
    group_online_global_search_sessions, list_authorized_global_search_sessions,
    reauthorize_global_search_sessions, send_global_search_cancellation_batches,
};
use crate::search::options::normalize_global_search_page_limits;
use crate::search::page_group::{GlobalSearchPage, search_worker_group};
use crate::search::worker_result::GlobalSearchSessionOutcome;

/// Refuse a `search_id` outside v2's `TerminalSearchIdSchema` (1..=64 UTF-16
/// code units, as a browser counts its strings).
pub fn require_search_id(search_id: &str) -> Result<(), ConnectError> {
    let length = search_id.encode_utf16().count();
    if (1..=TERMINAL_SEARCH_ID_MAX_LENGTH).contains(&length) {
        return Ok(());
    }
    Err(ConnectError::new(
        ErrorCode::InvalidArgument,
        "global search search_id must contain 1 to 64 characters",
    ))
}

/// A search is always tab-scoped: supersession can then only ever mean "this
/// tab replaced its own search" (v2 `requireSearchTabId`).
fn require_search_tab_id(caller: &Caller) -> Result<String, ConnectError> {
    let tab_id = caller.tab_id.as_deref().map(str::trim).unwrap_or_default();
    if tab_id.is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "terminal search requires the x-roost-tab-id header",
        ));
    }
    Ok(tab_id.to_owned())
}

fn cancelled() -> ConnectError {
    ConnectError::new(ErrorCode::Canceled, "global search cancelled")
}

/// Search one page of every authorized session's retained rows.
pub async fn handle_sessions_search_global(
    core: &CoordCore,
    caller: &Caller,
    req: SessionsSearchGlobalRequest,
) -> ServiceResult<SessionsSearchGlobalResponse> {
    let fingerprint = require_account_device(caller)?.to_owned();
    require_search_id(&req.search_id)?;
    if req.query.chars().count() > TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "global search query must contain at most 256 Unicode code points",
        ));
    }
    let limits = normalize_global_search_page_limits(
        req.max_sessions,
        req.max_rows_per_session,
        req.max_matches,
    );
    let tab_id = require_search_tab_id(caller)?;
    let search = &core.services.search;
    let page_deadline = search
        .lanes()
        .deadline_after(u64::from(GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS));
    let viewer_id = format!("{fingerprint}:{tab_id}");
    let identity = GlobalSearchIdentity {
        device_fingerprint: fingerprint,
        tab_id,
        search_id: req.search_id,
    };
    match search.cursors().begin_search(&identity) {
        GlobalSearchAdmission::Started => {}
        GlobalSearchAdmission::Capacity => {
            return Err(ConnectError::new(
                ErrorCode::ResourceExhausted,
                "too many active global searches",
            ));
        }
        GlobalSearchAdmission::Cancelled | GlobalSearchAdmission::Duplicate => {
            return Err(cancelled());
        }
    }
    let mut active = ActiveSearch {
        core,
        identity: &identity,
        viewer_id: &viewer_id,
        settled: false,
    };
    let binding = GlobalSearchCursorBinding {
        identity: identity.clone(),
        query: req.query,
        case_sensitive: req.case_sensitive,
        limits,
    };
    let page = GlobalSearchPage {
        core,
        identity: &identity,
        viewer_id: &viewer_id,
        query: &binding.query,
        case_sensitive: binding.case_sensitive,
        limits,
        page_deadline,
        cancellation: search.cursors().on_cancel(&identity),
    };
    let answered = search_page(&page, &binding, req.cursor).await;
    active.settled = true;
    answered
}

/// Ends the active record however the call ends. A call dropped before it
/// settled is v2's aborted request: the search is tombstoned and its selected
/// sessions are cancelled on their workers.
struct ActiveSearch<'a> {
    core: &'a CoordCore,
    identity: &'a GlobalSearchIdentity,
    viewer_id: &'a str,
    settled: bool,
}

impl Drop for ActiveSearch<'_> {
    fn drop(&mut self) {
        let cursors = self.core.services.search.cursors();
        if !self.settled {
            let cancellation = cursors.prepare_cancellation(self.identity);
            if cancellation.should_dispatch {
                tracing::info!(search_id = %self.identity.search_id, "global_search_aborted");
                let relay = &self.core.services.scrollback;
                send_global_search_cancellation_batches(
                    relay.workers(),
                    relay.pending(),
                    self.viewer_id,
                    &self.identity.search_id,
                    &cancellation.selected_sessions,
                );
                cursors.complete_cancellation(self.identity);
            }
        }
        cursors.finish_search(self.identity);
    }
}

async fn search_page(
    page: &GlobalSearchPage<'_>,
    binding: &GlobalSearchCursorBinding,
    cursor: Option<String>,
) -> ServiceResult<SessionsSearchGlobalResponse> {
    let cursors = page.core.services.search.cursors();
    let db = &page.core.services.db;
    let mut outcomes: HashMap<String, GlobalSearchSessionOutcome> = HashMap::new();
    let mut searched = SearchedSessions::default();
    let (page_sessions, authorized, eligible_sessions) = if let Some(token) = cursor {
        let progress = cursors.claim_cursor(&token, binding).ok_or_else(|| {
            ConnectError::new(
                ErrorCode::InvalidArgument,
                "global search cursor is invalid or expired",
            )
        })?;
        for session_id in progress.searched_session_ids {
            searched.insert(session_id);
        }
        if !cursors.select_sessions(page.identity, &progress.sessions) {
            return Err(cancelled());
        }
        let reauthorized = reauthorize_global_search_sessions(db, &progress.sessions).await?;
        for session_id in reauthorized.closed_session_ids {
            outcomes.insert(
                session_id,
                GlobalSearchSessionOutcome::unsearched(
                    Some(GlobalSearchPartialReason::SessionClosed),
                    None,
                ),
            );
        }
        if !cursors.select_sessions(page.identity, &reauthorized.authorized) {
            return Err(cancelled());
        }
        (
            progress.sessions,
            reauthorized.authorized,
            progress.eligible_sessions,
        )
    } else {
        let listed = list_authorized_global_search_sessions(db, page.limits.max_sessions).await?;
        if !cursors.select_sessions(page.identity, &listed.sessions) {
            return Err(cancelled());
        }
        (
            listed.sessions.clone(),
            listed.sessions,
            listed.eligible_sessions,
        )
    };
    if cursors.is_cancelled(page.identity) {
        return Err(cancelled());
    }
    let groups = group_online_global_search_sessions(
        page.core.services.scrollback.workers(),
        &authorized,
        &mut outcomes,
        page.limits.max_matches,
    );
    let settled = join_all(
        groups
            .into_iter()
            .map(|group| search_worker_group(page, group)),
    )
    .await;
    outcomes.extend(settled.into_iter().flatten());
    if cursors.is_cancelled(page.identity) {
        return Err(cancelled());
    }
    let ordered: Vec<(&GlobalSearchSessionPosition, GlobalSearchSessionOutcome)> = page_sessions
        .iter()
        .map(|session| {
            let outcome = outcomes
                .get(&session.session_id)
                .cloned()
                .unwrap_or_else(|| {
                    GlobalSearchSessionOutcome::unsearched(
                        Some(GlobalSearchPartialReason::MalformedResult),
                        None,
                    )
                });
            (session, outcome)
        })
        .collect();
    for (session, outcome) in &ordered {
        if outcome.searched {
            searched.insert(session.session_id.clone());
        }
    }
    let next_cursor = issue_next_cursor(page, binding, &ordered, eligible_sessions, &searched)?;
    let response = project_response(&ordered, next_cursor, &searched, eligible_sessions);
    tracing::info!(
        search_id = %page.identity.search_id,
        matches = response.matches.len(),
        partials = response.partials.len(),
        searched = response.searched_sessions,
        eligible = response.eligible_sessions,
        "global_search_page"
    );
    Response::ok(response)
}

/// The continuation cursor, when any session has more to scan. Sessions this
/// page never reached come first, so a retry is not starved by progress.
fn issue_next_cursor(
    page: &GlobalSearchPage<'_>,
    binding: &GlobalSearchCursorBinding,
    ordered: &[(&GlobalSearchSessionPosition, GlobalSearchSessionOutcome)],
    eligible_sessions: usize,
    searched: &SearchedSessions,
) -> Result<Option<String>, ConnectError> {
    let mut continuations: Vec<GlobalSearchContinuation> = ordered
        .iter()
        .filter_map(|(session, outcome)| {
            Some(GlobalSearchContinuation {
                position: outcome.continuation.clone()?,
                searched: outcome.searched,
                requested_before_row: session.before_row,
            })
        })
        .collect();
    if continuations.is_empty() {
        return Ok(None);
    }
    continuations.sort_by_key(|continuation| continuation.searched);
    let issued = page
        .core
        .services
        .search
        .cursors()
        .issue_cursor(GlobalSearchCursorIssue {
            binding: binding.clone(),
            continuations,
            eligible_sessions,
            searched_session_ids: searched.ordered.clone(),
        });
    issued.map(Some).map_err(|refusal| {
        tracing::error!(%refusal, "global_search_cursor_refused");
        let code = match refusal {
            CursorIssueRefusal::TokenUnavailable(_) => ErrorCode::Internal,
            _ => ErrorCode::Unknown,
        };
        ConnectError::new(code, refusal.to_string())
    })
}

fn project_response(
    ordered: &[(&GlobalSearchSessionPosition, GlobalSearchSessionOutcome)],
    next_cursor: Option<String>,
    searched: &SearchedSessions,
    eligible_sessions: usize,
) -> SessionsSearchGlobalResponse {
    let partials: Vec<SessionsSearchGlobalPartial> = ordered
        .iter()
        .filter_map(|(session, outcome)| {
            Some(SessionsSearchGlobalPartial {
                session_id: session.session_id.clone(),
                reason: outcome.partial_reason?.into(),
                ..Default::default()
            })
        })
        .collect();
    let matches = ordered
        .iter()
        .flat_map(|(_, outcome)| &outcome.matches)
        .map(|found| SessionsSearchGlobalMatch {
            session_id: found.session_id.clone(),
            row: found.row,
            col: found.col,
            len: found.len,
            preview: found.preview.clone(),
            grid_epoch: found.grid_epoch.clone(),
            ..Default::default()
        })
        .collect();
    let searched_count = searched.ordered.len();
    // Sessions past the page cap were never touched, so a page that searched
    // fewer than the eligible count is truncated even when nothing failed and
    // no continuation remains.
    let truncated =
        next_cursor.is_some() || !partials.is_empty() || searched_count < eligible_sessions;
    SessionsSearchGlobalResponse {
        matches,
        partials,
        next_cursor,
        searched_sessions: u32::try_from(searched_count).unwrap_or(u32::MAX),
        eligible_sessions: u32::try_from(eligible_sessions).unwrap_or(u32::MAX),
        truncated,
        ..Default::default()
    }
}

/// Every session this search has scanned, in first-scanned order.
#[derive(Debug, Default)]
struct SearchedSessions {
    ordered: Vec<String>,
    seen: HashSet<String>,
}

impl SearchedSessions {
    fn insert(&mut self, session_id: String) {
        if self.seen.insert(session_id.clone()) {
            self.ordered.push(session_id);
        }
    }
}
