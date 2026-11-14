//! Navigation queries normalize terms and match them across projected fields.
//! The search and palette share the same matching behavior.

use roost_client_core::store::navigation::query::{
    document_matches_terms, filter_navigation_search_documents, navigation_search_terms,
};
use roost_client_core::store::navigation::{NavigationSearchDocument, session_href};
use roost_client_core::store::palette::matches_query;

fn document(session_id: &str) -> NavigationSearchDocument {
    NavigationSearchDocument {
        session_id: session_id.to_owned(),
        href: session_href(session_id),
        display_title: session_id.to_owned(),
        custom_title: None,
        terminal_title: None,
        cwd: "/home/dev".to_owned(),
        spawn_cwd: None,
        workspace_id: None,
        workspace_name: None,
        folder_key: "workstation::/home/dev".to_owned(),
        worker_label: "workstation".to_owned(),
        worker_fp: "machine".to_owned(),
        git_branch: None,
        git_remote: None,
        pull_request_number: None,
        pull_request_state: None,
        pull_request_checks: None,
        pull_request_url: None,
        port_label: None,
        search_text: session_id.to_owned(),
        activity_at: 1,
        available: true,
        client_only: false,
    }
}

#[test]
fn every_query_term_must_match_and_may_match_different_fields() {
    let rows = vec![document("api gateway")];
    assert_eq!(
        filter_navigation_search_documents(&rows, "  GATEWAY   api  ").len(),
        1,
        "both terms match, and neither had to be in the same field"
    );
    assert!(filter_navigation_search_documents(&rows, "gateway nope").is_empty());
    assert_eq!(
        filter_navigation_search_documents(&rows, "   ").len(),
        1,
        "an empty query is the whole list, not an empty result"
    );
    assert_eq!(
        navigation_search_terms("  Main   API "),
        vec!["main", "api"]
    );
    assert!(matches_query(
        "Main API",
        &navigation_search_terms("main api")
    ));
    assert!(
        matches_query("anything", &[]),
        "no terms matches everything"
    );
    assert!(document_matches_terms(
        &rows[0],
        &navigation_search_terms("api")
    ));
}
