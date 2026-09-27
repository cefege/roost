//! Reconciling a cursor-paged fleet-wide answer into a published list.
//!
//! The rows themselves live in `crate::search::global`; what lives here is the
//! client's judgement about them — which duplicates a page may contain, which
//! partials a continuation replaces, and which rows are still navigable. The
//! split is the boundary: this module decides, `crate::search::global` holds.

use std::collections::BTreeSet;

use roost_protocol::wire::SessionId;

use crate::search::global::{GlobalSearchMatch, GlobalSearchPartial};

/// Merge an incoming page into what is already published.
///
/// Pages tile a cursor, so a row can legitimately appear on two of them when
/// the coordinator's own scan moves underneath the cursor. Deduplicating by
/// identity is what keeps the list from showing one hit twice.
#[must_use]
pub fn merge_global_search_matches(
    current: &[GlobalSearchMatch],
    incoming: &[GlobalSearchMatch],
) -> Vec<GlobalSearchMatch> {
    let mut merged = current.to_vec();
    let mut identities: BTreeSet<String> =
        current.iter().map(GlobalSearchMatch::identity).collect();
    for candidate in incoming {
        if identities.insert(candidate.identity()) {
            merged.push(candidate.clone());
        }
    }
    merged
}

/// Fold an incoming page's partials into what is already published.
///
/// A successful continuation REPLACES the retryable partials, because a
/// continuation is a fresh attempt at the same sessions, and KEEPS the final
/// ones, because a cap or an eviction is a property of the session that a
/// second attempt does not undo.
#[must_use]
pub fn reconcile_global_search_partials(
    current: &[GlobalSearchPartial],
    incoming: &[GlobalSearchPartial],
) -> Vec<GlobalSearchPartial> {
    let retained: Vec<GlobalSearchPartial> = current
        .iter()
        .filter(|partial| partial.reason.is_terminal())
        .cloned()
        .collect();
    let mut merged = retained.clone();
    let mut identities: BTreeSet<String> =
        retained.iter().map(GlobalSearchPartial::identity).collect();
    for candidate in incoming {
        if identities.insert(candidate.identity()) {
            merged.push(candidate.clone());
        }
    }
    merged
}

/// Drop the matches whose session the client no longer holds a row for.
///
/// A global search outlives the session list it was started against, and a row
/// whose session is gone has nowhere to navigate to. Rendering it anyway is how
/// a search result becomes a link to nothing.
#[must_use]
pub fn retain_joinable_matches(
    matches: Vec<GlobalSearchMatch>,
    known_sessions: &BTreeSet<SessionId>,
) -> Vec<GlobalSearchMatch> {
    matches
        .into_iter()
        .filter(|candidate| known_sessions.contains(&candidate.session_id))
        .collect()
}
