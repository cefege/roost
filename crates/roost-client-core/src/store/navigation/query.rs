//! The query half of navigation search: normalize once, match every term, and
//! order the rows that want attention.
//!
//! Split from the projection because the two change for different reasons: a new
//! field on a session is a projection change, and a new matching rule is a query
//! change. What they must agree on is the NORMALIZER, so both call the one
//! function here — an index built with one normalizer and searched with another
//! is a search that silently misses.
//!
//! Every term must match, and they may match in different fields: "roost api
//! blocked" is a query about a machine, a folder and a status at once, and
//! requiring one field to contain the whole phrase is what makes a filter feel
//! broken.

use crate::store::navigation::{NavigationSearchDocument, attention_rank};

/// Normalize a query or a document's search text, once, for every consumer.
///
/// Lowercase, trim, and collapse internal whitespace runs. Deliberately NOT NFKC:
/// see the deviation note in the parent module.
pub fn normalize_navigation_search_query(value: &str) -> String {
    value
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
}

/// The query's terms, already normalized. Split ONCE per filter run — matching
/// re-normalizes the candidate, never the query.
pub fn navigation_search_terms(query: &str) -> Vec<String> {
    let normalized = normalize_navigation_search_query(query);
    if normalized.is_empty() {
        return Vec::new();
    }
    normalized.split(' ').map(str::to_owned).collect()
}

/// Whether a document matches a query, normalizing the query first.
pub fn matches_navigation_search_document(
    document: &NavigationSearchDocument,
    query: &str,
) -> bool {
    document_matches_terms(document, &navigation_search_terms(query))
}

/// The rows a query selects, in the order they came.
///
/// An EMPTY QUERY SELECTS EVERYTHING. That is the whole search page with no box
/// typed into it, and a filter that returned nothing for it would look like a
/// broken index rather than an empty one.
pub fn filter_navigation_search_documents<'a>(
    documents: &'a [NavigationSearchDocument],
    query: &str,
) -> Vec<&'a NavigationSearchDocument> {
    let terms = navigation_search_terms(query);
    if terms.is_empty() {
        return documents.iter().collect();
    }
    documents
        .iter()
        .filter(|document| document_matches_terms(document, &terms))
        .collect()
}

/// The rows that want attention, in the order an operator should answer them.
///
/// Selects WITHOUT acknowledging: navigation stays the seen-state owner, and a
/// filter that marked rows seen would make the attention list empty the moment it
/// was opened.
pub fn attention_navigation_documents(
    documents: &[NavigationSearchDocument],
) -> Vec<&NavigationSearchDocument> {
    let mut attention: Vec<&NavigationSearchDocument> = documents
        .iter()
        .filter(|document| document.agent_attention.is_some())
        .collect();
    attention.sort_by(|left, right| compare_attention_documents(left, right));
    attention
}

/// Whether a document matches an already-split, already-normalized term list.
///
/// The term list comes from [`navigation_search_terms`], so a caller that filters
/// once per keystroke splits once rather than once per row.
pub fn document_matches_terms(document: &NavigationSearchDocument, terms: &[String]) -> bool {
    terms
        .iter()
        .all(|term| document.search_text.contains(term.as_str()))
}

/// A session an operator cannot reach cannot be answered, so an unavailable
/// worker's row sinks below every reachable one whatever its level. Recency is
/// the BROWSER'S arrival counter, never a worker clock: worker wall clocks are
/// unsynchronized, and one machine running minutes ahead would otherwise own the
/// top of the list.
fn compare_attention_documents(
    left: &NavigationSearchDocument,
    right: &NavigationSearchDocument,
) -> std::cmp::Ordering {
    u8::from(!left.available)
        .cmp(&u8::from(!right.available))
        .then_with(|| attention_rank(left).cmp(&attention_rank(right)))
        .then_with(|| right.agent_unseen.cmp(&left.agent_unseen))
        .then_with(|| right.agent_arrival.cmp(&left.agent_arrival))
        .then_with(|| left.session_id.cmp(&right.session_id))
}
