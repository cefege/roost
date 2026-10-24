//! Fleet result projection and reader-facing coverage messages for terminal search.
//! `content::ContentResults` owns loading and rendering; these functions keep
//! joining and summary semantics independently addressable and tested.

use roost_client_core::client::global_search::GlobalSearchResults;
use roost_client_core::search::global::GlobalSearchPartial;
use roost_client_core::store::navigation::NavigationSearchDocument;

use super::JoinedMatch;

/// The matches, each joined to the session row the projection knows.
#[must_use]
pub fn join_matches(
    matches: &[roost_client_core::search::global::GlobalSearchMatch],
    documents: &[NavigationSearchDocument],
) -> Vec<JoinedMatch> {
    matches
        .iter()
        .map(|candidate| JoinedMatch {
            candidate: candidate.clone(),
            document: documents
                .iter()
                .find(|document| document.session_id == candidate.session_id.as_str())
                .cloned(),
        })
        .collect()
}

/// Whether the answer is anything short of the whole fleet.
#[must_use]
pub fn incomplete(results: &GlobalSearchResults, missing_projections: usize) -> bool {
    results.truncated
        || results.next_cursor.is_some()
        || !results.partials.is_empty()
        || missing_projections > 0
        || (results.eligible_sessions > 0 && results.searched_sessions < results.eligible_sessions)
        || (results.error.is_some() && !results.matches.is_empty())
}

/// The sentence under the panel's heading.
#[must_use]
pub fn summary(
    results: &GlobalSearchResults,
    rows: &[JoinedMatch],
    waiting: bool,
    query: &str,
) -> String {
    if query.trim().is_empty() {
        return "Search retained terminal content with the field above.".to_owned();
    }
    if waiting && !results.has_searched {
        return "Searching retained terminal content…".to_owned();
    }
    if let Some(error) = results.error.as_ref() {
        return format!("Terminal content search failed: {error}");
    }
    let matches = rows.iter().filter(|row| row.document.is_some()).count();
    let noun = if matches == 1 { "match" } else { "matches" };
    if results.eligible_sessions == 0 {
        return format!("{matches} {noun}");
    }
    if results.searched_sessions < results.eligible_sessions {
        format!(
            "{matches} {noun} across {} of {} sessions — coverage is partial",
            results.searched_sessions, results.eligible_sessions
        )
    } else {
        format!(
            "{matches} {noun} across all {} sessions searched",
            results.eligible_sessions
        )
    }
}

/// One partial, named for the session it belongs to.
#[must_use]
pub fn partial_line(
    partial: &GlobalSearchPartial,
    documents: &[NavigationSearchDocument],
) -> String {
    let name = documents
        .iter()
        .find(|document| document.session_id == partial.session_id.as_str())
        .map(|document| document.display_title.clone())
        .unwrap_or_else(|| "A session no longer in the current view".to_owned());
    format!("{name} {}.", partial.reason.label())
}
