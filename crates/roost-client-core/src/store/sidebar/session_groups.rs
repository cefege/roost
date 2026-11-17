//! The Sessions panel's dynamic projection: every live terminal destination,
//! newest activity first, bucketed by the local day it last ran. Reads the one
//! navigation index (`sidebar::documents`) and the shared query matcher; the
//! web SessionsView renders it.

use roost_protocol::wire::SessionKind;

use crate::store::Store;
use crate::store::navigation::query::{document_matches_terms, navigation_search_terms};
use crate::store::paths::WorkerPaths;
use crate::store::pending_close::is_pending_close;
use crate::store::selectors::session_by_id;
use crate::store::sidebar::documents::store_navigation_documents;

/// One day bucket of the Sessions panel, newest day first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGroup {
    /// The bucket's heading: Today, Yesterday or Earlier.
    pub label: &'static str,
    /// The bucket's sessions, newest activity first.
    pub session_ids: Vec<String>,
}

const DAY_MS: i64 = 86_400_000;
const DAY_LABELS: [&str; 3] = ["Today", "Yesterday", "Earlier"];

/// Bucket the live shell destinations by local day, keeping the index's
/// newest-first order. `utc_offset_minutes` is the reader's offset east of
/// UTC, so a day boundary falls where their clock says it does.
pub fn build_session_groups(
    store: &Store,
    paths: &dyn WorkerPaths,
    now_ms: i64,
    utc_offset_minutes: i32,
    query: &str,
) -> Vec<SessionGroup> {
    let documents = store_navigation_documents(store, paths, now_ms);
    let terms = navigation_search_terms(query);
    let mut buckets: [Vec<String>; 3] = Default::default();
    for document in &documents {
        // The Folders panel's membership: a live shell row, not a placeholder
        // this browser minted and not one waiting out a close.
        if document.client_only {
            continue;
        }
        let Some(session) = session_by_id(store, &document.session_id) else {
            continue;
        };
        if session.kind != SessionKind::Shell || is_pending_close(store, &document.session_id) {
            continue;
        }
        if !terms.is_empty() && !document_matches_terms(document, &terms) {
            continue;
        }
        buckets[day_bucket(document.activity_at, now_ms, utc_offset_minutes)]
            .push(document.session_id.clone());
    }
    DAY_LABELS
        .into_iter()
        .zip(buckets)
        .filter(|(_, session_ids)| !session_ids.is_empty())
        .map(|(label, session_ids)| SessionGroup { label, session_ids })
        .collect()
}

/// Which local day an activity stamp falls in: 0 today, 1 yesterday, 2 earlier.
fn day_bucket(activity_ms: i64, now_ms: i64, utc_offset_minutes: i32) -> usize {
    let offset_ms = i64::from(utc_offset_minutes) * 60_000;
    let local_now = now_ms + offset_ms;
    let today_start = local_now - local_now.rem_euclid(DAY_MS) - offset_ms;
    if activity_ms >= today_start {
        0
    } else if activity_ms >= today_start - DAY_MS {
        1
    } else {
        2
    }
}
