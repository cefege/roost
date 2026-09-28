//! `SessionsSearchGlobalResponse` → the global search controller's page.
//!
//! Called by `codec::response` for `RpcCall::SessionsSearchGlobal`; the page is
//! folded by `client::global_search`. v2 read the response fields verbatim
//! (`apps/web/src/lib/globalContentSearchController.ts:216-234`); the one
//! difference is that a row naming a session id that fails the brand check is
//! dropped here, because the controller's rows are keyed by the branded id.

use roost_proto::{
    GlobalSearchPartialReason as PbPartialReason, SessionsSearchGlobalResponse,
};
use roost_protocol::wire::SessionId;

use crate::search::global::{
    GlobalSearchMatch, GlobalSearchPartial, GlobalSearchPartialReason, GlobalSearchResponse,
};

/// One page, as the controller folds it.
pub(super) fn search_page_from_proto(response: &SessionsSearchGlobalResponse) -> GlobalSearchResponse {
    let matches = response
        .matches
        .iter()
        .filter_map(|row| {
            let session_id = branded(&row.session_id)?;
            Some(GlobalSearchMatch {
                session_id,
                row: row.row,
                col: row.col,
                len: row.len,
                preview: row.preview.clone(),
                grid_epoch: row.grid_epoch.clone(),
            })
        })
        .collect();
    let partials = response
        .partials
        .iter()
        .filter_map(|partial| {
            Some(GlobalSearchPartial {
                session_id: branded(&partial.session_id)?,
                reason: partial_reason(partial.reason.as_known()),
            })
        })
        .collect();
    GlobalSearchResponse {
        matches,
        partials,
        next_cursor: response.next_cursor.clone(),
        searched_sessions: response.searched_sessions,
        eligible_sessions: response.eligible_sessions,
        truncated: response.truncated,
    }
}

fn branded(session_id: &str) -> Option<SessionId> {
    SessionId::try_from(session_id)
        .map_err(|error| {
            tracing::warn!(
                target: "sync",
                session_id,
                %error,
                "global search row names an invalid session id; dropped"
            );
        })
        .ok()
}

/// A reason this build does not know reads as unspecified, which the controller
/// renders as an incomplete result rather than hiding the session.
fn partial_reason(reason: Option<PbPartialReason>) -> GlobalSearchPartialReason {
    match reason {
        None | Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_UNSPECIFIED) => {
            GlobalSearchPartialReason::Unspecified
        }
        Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_WORKER_UNAVAILABLE) => {
            GlobalSearchPartialReason::WorkerUnavailable
        }
        Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_DEADLINE) => {
            GlobalSearchPartialReason::Deadline
        }
        Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_EPOCH_CHANGED) => {
            GlobalSearchPartialReason::EpochChanged
        }
        Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_MATCH_LIMIT) => {
            GlobalSearchPartialReason::MatchLimit
        }
        Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_HISTORY_EVICTED) => {
            GlobalSearchPartialReason::HistoryEvicted
        }
        Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_SESSION_CLOSED) => {
            GlobalSearchPartialReason::SessionClosed
        }
        Some(PbPartialReason::GLOBAL_SEARCH_PARTIAL_REASON_MALFORMED_RESULT) => {
            GlobalSearchPartialReason::MalformedResult
        }
    }
}
