//! One worker's share of a global-search page: wait for its lane, send one
//! `search-scrollback-batch`, settle the reply within the page deadline, and
//! turn the answer (or its absence) into per-session outcomes.
//!
//! Ports the per-group body of `sessionsSearchGlobal` in
//! `apps/coord/src/search/handlers-sessions-global-search.ts`. Called by
//! `search::rpc_search` once per online worker, concurrently; correlates
//! through `services.scrollback.pending()` and sends through the one gate,
//! `workers::send::send_frame_through`.

use std::time::Duration;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::GlobalSearchPartialReason;
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS, GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS,
};
use roost_protocol::wire::SessionId;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::control::global_search::{
    GlobalSearchSession, GlobalSearchSessions, TerminalSearchGridEpoch, TerminalSearchId,
    TerminalSearchQuery, TerminalSearchRow,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::coord_core::CoordCore;
use crate::search::cursor_types::GlobalSearchIdentity;
use crate::search::fanout::OnlineGlobalSearchGroup;
use crate::search::options::GlobalSearchPageLimits;
use crate::search::worker_result::{
    GlobalSearchSessionOutcome, outcome_for_global_search_entry,
    validate_global_search_group_result,
};
use crate::workers::send::{SendOutcome, send_frame_through};

/// Everything one page's groups share.
pub struct GlobalSearchPage<'a> {
    pub core: &'a CoordCore,
    pub identity: &'a GlobalSearchIdentity,
    pub viewer_id: &'a str,
    pub query: &'a str,
    pub case_sensitive: bool,
    pub limits: GlobalSearchPageLimits,
    pub page_deadline: Instant,
    /// Fired by a completed cancellation of this search.
    pub cancellation: CancellationToken,
}

impl std::fmt::Debug for GlobalSearchPage<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GlobalSearchPage")
            .field("identity", self.identity)
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

/// The outcomes one group settles, keyed by session id. Empty when the search
/// was cancelled: the page is refused as a whole, so nothing is recorded.
pub async fn search_worker_group(
    page: &GlobalSearchPage<'_>,
    group: OnlineGlobalSearchGroup,
) -> Vec<(String, GlobalSearchSessionOutcome)> {
    let search = &page.core.services.search;
    let work = Duration::from_millis(u64::from(GLOBAL_TERMINAL_SEARCH_WORK_DEADLINE_MS));
    let queue_deadline = page
        .page_deadline
        .checked_sub(work)
        .unwrap_or_else(Instant::now);
    let lease = search
        .lanes()
        .acquire(group.worker_fp.as_str(), queue_deadline, &page.cancellation)
        .await;
    let Some(_lease) = lease else {
        if search.cursors().is_cancelled(page.identity) {
            return Vec::new();
        }
        tracing::debug!(worker_fp = %group.worker_fp, "global_search_lane_deadline");
        return every_session(&group, GlobalSearchPartialReason::Deadline);
    };
    if search.cursors().is_cancelled(page.identity) {
        return Vec::new();
    }
    let remaining = search.lanes().remaining(page.page_deadline);
    if remaining < work {
        return every_session(&group, GlobalSearchPartialReason::Deadline);
    }
    let relay = &page.core.services.scrollback;
    let worker_fp = group.worker_fp.as_str();
    let mut pending = match relay
        .pending()
        .create_fresh(Some(worker_fp), relay.now_ms())
    {
        Ok(pending) => pending,
        Err(error) => return every_session(&group, reason_for_error(&error)),
    };
    let sent = batch_frame(page, &group, pending.request_id()).and_then(|frame| {
        let downstream = CoordWorkerDownstream::BrowserCommand {
            browser_id: page.viewer_id.to_owned(),
            viewer_id: page.viewer_id.to_owned(),
            request_id: pending.request_id().to_owned(),
            frame,
            trace_id: None,
        };
        match send_frame_through(relay.workers(), &group.handle, downstream) {
            SendOutcome::Admitted { .. } => Ok(()),
            SendOutcome::Refused(refusal) => Err(format!("send failed: {refusal}")),
        }
    });
    match &sent {
        Ok(()) => tracing::info!(
            worker_fp,
            search_id = %page.identity.search_id,
            sessions = group.sessions.len(),
            "global_search_batch_sent"
        ),
        Err(error) => {
            tracing::warn!(worker_fp, %error, "global_search_batch_send_failed");
            relay.pending().reject_unavailable(
                pending.request_id(),
                &format!("global search send failed: {error}"),
                Some(worker_fp),
            );
        }
    }
    let settled = tokio::select! {
        biased;
        () = page.cancellation.cancelled() => return Vec::new(),
        settled = tokio::time::timeout(remaining, pending.settle()) => settled,
    };
    let raw = match settled {
        Ok(Ok(raw)) => raw,
        Ok(Err(error)) => {
            if search.cursors().is_cancelled(page.identity) {
                return Vec::new();
            }
            return every_session(&group, reason_for_error(&error));
        }
        Err(_elapsed) => {
            if search.cursors().is_cancelled(page.identity) {
                return Vec::new();
            }
            return every_session(&group, GlobalSearchPartialReason::Deadline);
        }
    };
    let Some(entries) = validate_global_search_group_result(
        &raw,
        &group.sessions,
        group.match_budget,
        &page.limits,
    ) else {
        tracing::warn!(worker_fp, "global_search_batch_malformed");
        return every_session(&group, GlobalSearchPartialReason::MalformedResult);
    };
    entries
        .iter()
        .zip(&group.sessions)
        .map(|(entry, session)| {
            (
                entry.session_id().to_owned(),
                outcome_for_global_search_entry(session, entry),
            )
        })
        .collect()
}

/// The same unsearched partial for every session of a group, each retrying
/// its own position.
fn every_session(
    group: &OnlineGlobalSearchGroup,
    reason: GlobalSearchPartialReason,
) -> Vec<(String, GlobalSearchSessionOutcome)> {
    group
        .sessions
        .iter()
        .map(|session| {
            (
                session.session_id.clone(),
                GlobalSearchSessionOutcome::unsearched(Some(reason), Some(session.clone())),
            )
        })
        .collect()
}

/// A settled failure, in partial vocabulary.
fn reason_for_error(error: &ConnectError) -> GlobalSearchPartialReason {
    match error.code {
        ErrorCode::DeadlineExceeded => GlobalSearchPartialReason::Deadline,
        ErrorCode::Unavailable => GlobalSearchPartialReason::WorkerUnavailable,
        _ => GlobalSearchPartialReason::MalformedResult,
    }
}

/// The `search-scrollback-batch` frame for one group, or why it cannot be
/// branded (a value the worker's own decoder would refuse).
fn batch_frame(
    page: &GlobalSearchPage<'_>,
    group: &OnlineGlobalSearchGroup,
    request_id: &str,
) -> Result<ClientControlFrame, String> {
    let brand = |error: roost_protocol::ProtocolError| error.to_string();
    let sessions = group
        .sessions
        .iter()
        .map(|session| {
            let before_row = session
                .before_row
                .map(|row| {
                    let row = i64::try_from(row).map_err(|error| error.to_string())?;
                    TerminalSearchRow::try_from(row).map_err(brand)
                })
                .transpose()?;
            Ok(GlobalSearchSession {
                session_id: SessionId::try_from(session.session_id.as_str()).map_err(brand)?,
                grid_epoch: TerminalSearchGridEpoch::try_from(session.grid_epoch.as_str())
                    .map_err(brand)?,
                before_row,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ClientControlFrame::SearchScrollbackBatch {
        request_id: request_id.to_owned(),
        search_id: TerminalSearchId::try_from(page.identity.search_id.as_str()).map_err(brand)?,
        query: TerminalSearchQuery::try_from(page.query).map_err(brand)?,
        case_sensitive: page.case_sensitive,
        sessions: GlobalSearchSessions::try_from(sessions).map_err(brand)?,
        max_rows_per_session: i64::from(page.limits.max_rows_per_session),
        max_matches: i64::from(group.match_budget),
        deadline_ms: i64::from(GLOBAL_TERMINAL_SEARCH_PAGE_DEADLINE_MS),
        trace_id: None,
    })
}
