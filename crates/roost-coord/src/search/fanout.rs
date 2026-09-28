//! Authorized session selection, worker grouping, and cancellation fan-out for
//! install-wide terminal search.
//!
//! Ports `listAuthorizedGlobalSearchSessions`, `reauthorizeGlobalSearchSessions`,
//! `groupOnlineGlobalSearchSessions` and `sendGlobalSearchCancellationBatches`
//! from `apps/coord/src/search/global-search-fanout.ts` (result validation is
//! `search::worker_result`). Called by `search::rpc_search` and
//! `search::cancel`; frames leave through `workers::send`, the one send gate.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use connectrpc::{ConnectError, ErrorCode};
use roost_proto::GlobalSearchPartialReason;
use roost_protocol::terminal_search::GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS;
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::control::global_search::{GlobalSearchSessionIds, TerminalSearchId};
use roost_protocol::wire::{SessionId, WorkerFp};

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::db::CoordDb;
use crate::search::cursor_types::GlobalSearchSessionPosition;
use crate::search::options::allocate_global_search_match_limits;
use crate::search::worker_result::GlobalSearchSessionOutcome;
use crate::terminal_screen::pending_rpcs::PendingRpcs;
use crate::workers::send::{SendOutcome, send_browser_command};

/// One page's authorized sessions and the exact count they were drawn from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedGlobalSearchPage {
    /// Newest-first sessions this page may search, capped at `max_sessions`.
    pub sessions: Vec<GlobalSearchSessionPosition>,
    /// The count over the same predicate. The page cap bounds the work, never
    /// the denominator: reporting the cap told the browser a 100-session
    /// install was fully searched after 32.
    pub eligible_sessions: usize,
}

/// A cursor's sessions after the authorization predicate was re-applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReauthorizedGlobalSearchSessions {
    /// Still open, on the same undeleted worker.
    pub authorized: Vec<GlobalSearchSessionPosition>,
    /// Closed, moved, or on a deleted worker.
    pub closed_session_ids: Vec<String>,
}

/// One routable worker's share of a page.
#[derive(Debug, Clone)]
pub struct OnlineGlobalSearchGroup {
    /// The worker.
    pub worker_fp: WorkerFp,
    /// The generation that was routable when the page was grouped.
    pub handle: Arc<WorkerHandle>,
    /// The sessions it scans, in page order.
    pub sessions: Vec<GlobalSearchSessionPosition>,
    /// The sum of those sessions' match budgets.
    pub match_budget: u32,
}

/// The single authorization predicate for install-wide search. The window
/// count is taken over the same rows BEFORE the limit, so the page and its
/// denominator cannot drift apart.
const AUTHORIZED_PAGE_SQL: &str = "SELECT session.id, session.worker_fp, COUNT(*) OVER () \
     FROM sessions AS session \
     INNER JOIN workers AS worker ON worker.fp = session.worker_fp \
     WHERE session.status = 'open' AND worker.deleted_at_ms IS NULL \
     ORDER BY session.created_at DESC, session.id DESC LIMIT ?1";

const REAUTHORIZE_SQL: &str = "SELECT session.id, session.worker_fp \
     FROM sessions AS session \
     INNER JOIN workers AS worker ON worker.fp = session.worker_fp \
     WHERE session.id IN (SELECT value FROM json_each(?1)) \
     AND session.status = 'open' AND worker.deleted_at_ms IS NULL";

/// The newest `max_sessions` open sessions on undeleted workers.
pub async fn list_authorized_global_search_sessions(
    db: &CoordDb,
    max_sessions: usize,
) -> Result<AuthorizedGlobalSearchPage, ConnectError> {
    let rows: Vec<(String, String, i64)> = sqlx::query_as(AUTHORIZED_PAGE_SQL)
        .bind(i64::try_from(max_sessions).unwrap_or(i64::MAX))
        .fetch_all(db.pool())
        .await
        .map_err(lookup_failed)?;
    let counted = rows.first().map_or(0, |(_, _, eligible)| {
        usize::try_from(*eligible).unwrap_or(0)
    });
    Ok(AuthorizedGlobalSearchPage {
        eligible_sessions: counted.max(rows.len()),
        sessions: rows
            .into_iter()
            .map(|(session_id, worker_fp, _)| {
                GlobalSearchSessionPosition::unvisited(session_id, worker_fp)
            })
            .collect(),
    })
}

/// Re-apply the predicate to a cursor's sessions: a session closed, moved to
/// another worker, or whose worker was deleted since the cursor was issued
/// is not searched again.
pub async fn reauthorize_global_search_sessions(
    db: &CoordDb,
    positions: &[GlobalSearchSessionPosition],
) -> Result<ReauthorizedGlobalSearchSessions, ConnectError> {
    let ids: Vec<&str> = positions.iter().map(|p| p.session_id.as_str()).collect();
    let rows: Vec<(String, String)> = sqlx::query_as(REAUTHORIZE_SQL)
        .bind(serde_json::Value::from(ids).to_string())
        .fetch_all(db.pool())
        .await
        .map_err(lookup_failed)?;
    let worker_by_session: HashMap<String, String> = rows.into_iter().collect();
    let mut result = ReauthorizedGlobalSearchSessions {
        authorized: Vec::new(),
        closed_session_ids: Vec::new(),
    };
    let mut seen: HashSet<&str> = HashSet::new();
    for position in positions {
        if !seen.insert(position.session_id.as_str()) {
            continue;
        }
        if worker_by_session.get(&position.session_id) == Some(&position.worker_fp) {
            result.authorized.push(position.clone());
        } else {
            result.closed_session_ids.push(position.session_id.clone());
        }
    }
    Ok(result)
}

/// Group a page's sessions by routable worker and split the match budget.
///
/// A session on an offline worker is a `WORKER_UNAVAILABLE` partial that
/// retries its own position; a session whose budget share is zero is deferred
/// to the next page with no partial.
pub fn group_online_global_search_sessions(
    registry: &WorkerRegistry,
    sessions: &[GlobalSearchSessionPosition],
    outcomes: &mut HashMap<String, GlobalSearchSessionOutcome>,
    max_matches: u32,
) -> Vec<OnlineGlobalSearchGroup> {
    let mut by_worker: Vec<(String, Vec<GlobalSearchSessionPosition>)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for session in sessions {
        if !seen.insert(session.session_id.as_str()) {
            continue;
        }
        match by_worker
            .iter_mut()
            .find(|(fp, _)| *fp == session.worker_fp)
        {
            Some((_, group)) => group.push(session.clone()),
            None => by_worker.push((session.worker_fp.clone(), vec![session.clone()])),
        }
    }
    let mut online: Vec<(
        WorkerFp,
        Arc<WorkerHandle>,
        Vec<GlobalSearchSessionPosition>,
    )> = Vec::new();
    for (worker_fp, worker_sessions) in by_worker {
        let routable = WorkerFp::try_from(worker_fp.as_str())
            .ok()
            .and_then(|fp| registry.current_routable(&fp).map(|handle| (fp, handle)));
        if let Some((fp, handle)) = routable {
            online.push((fp, handle, worker_sessions));
            continue;
        }
        for session in worker_sessions {
            outcomes.insert(
                session.session_id.clone(),
                GlobalSearchSessionOutcome::unsearched(
                    Some(GlobalSearchPartialReason::WorkerUnavailable),
                    Some(session),
                ),
            );
        }
    }
    let session_count: usize = online.iter().map(|(_, _, group)| group.len()).sum();
    if session_count == 0 {
        return Vec::new();
    }
    let mut budgets = allocate_global_search_match_limits(max_matches, session_count).into_iter();
    let mut scheduled = Vec::new();
    for (worker_fp, handle, worker_sessions) in online {
        let mut match_budget = 0_u32;
        let mut selected = Vec::new();
        for session in worker_sessions {
            let budget = budgets.next().unwrap_or_default();
            if budget > 0 {
                selected.push(session);
                match_budget += budget;
            } else {
                outcomes.insert(
                    session.session_id.clone(),
                    GlobalSearchSessionOutcome::unsearched(None, Some(session)),
                );
            }
        }
        if !selected.is_empty() {
            scheduled.push(OnlineGlobalSearchGroup {
                worker_fp,
                handle,
                sessions: selected,
                match_budget,
            });
        }
    }
    scheduled
}

/// Tell every routable worker to stop scanning its sessions of one search:
/// one batch per worker, sessions de-duplicated and capped at a page. Fire
/// and forget; a failed send is logged, never raised.
pub fn send_global_search_cancellation_batches(
    registry: &WorkerRegistry,
    pending: &PendingRpcs,
    viewer_id: &str,
    search_id: &str,
    sessions: &[GlobalSearchSessionPosition],
) {
    let mut by_worker: Vec<(&str, Vec<SessionId>)> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for session in sessions {
        if seen.len() >= GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS
            || !seen.insert(session.session_id.as_str())
        {
            continue;
        }
        let Ok(session_id) = SessionId::try_from(session.session_id.as_str()) else {
            continue;
        };
        match by_worker
            .iter_mut()
            .find(|(fp, _)| *fp == session.worker_fp)
        {
            Some((_, ids)) => ids.push(session_id),
            None => by_worker.push((session.worker_fp.as_str(), vec![session_id])),
        }
    }
    let Ok(branded_search_id) = TerminalSearchId::try_from(search_id) else {
        return;
    };
    for (worker_fp, session_ids) in by_worker {
        let Ok(fingerprint) = WorkerFp::try_from(worker_fp) else {
            continue;
        };
        if registry.current_routable(&fingerprint).is_none() {
            continue;
        }
        let Ok(session_ids) = GlobalSearchSessionIds::try_from(session_ids) else {
            continue;
        };
        let request_id = pending.next_request_id();
        let frame = ClientControlFrame::CancelScrollbackSearchBatch {
            request_id: request_id.clone(),
            search_id: branded_search_id.clone(),
            session_ids,
            trace_id: None,
        };
        match send_browser_command(
            registry,
            &fingerprint,
            viewer_id,
            viewer_id,
            &request_id,
            frame,
        ) {
            SendOutcome::Admitted { .. } => {
                tracing::info!(worker_fp, search_id, "global_search_cancel_sent");
            }
            SendOutcome::Refused(refusal) => {
                tracing::warn!(worker_fp, search_id, %refusal, "global_search_cancel_send_failed");
            }
        }
    }
}

fn lookup_failed(error: sqlx::Error) -> ConnectError {
    tracing::error!(%error, "global search session lookup failed");
    ConnectError::new(ErrorCode::Internal, "global search session lookup failed")
}
