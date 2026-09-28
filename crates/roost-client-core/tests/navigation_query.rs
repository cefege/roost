//! The query half of navigation search: one normalizer on both sides, every term
//! must match, and the attention order comes from the shared level vocabulary
//! rather than from this crate's own enum.
//!
//! The vocabulary mapping is the seam with the agent-status module, and the sort
//! is keyed on the TOKEN rather than on the enum for a reason: an enum-keyed sort
//! would put a blocked row first today and keep doing so after a fourth attention
//! state arrived with a different order, with neither this crate's tests nor the
//! agent module's noticing.
//!
//! The mutation experiment for this file is in the slice report: in
//! `attention_for_level_token`, map `"done"` to `None`, and
//! `every_agent_level_token_maps_onto_exactly_one_attention_state` must fail.

use roost_client_core::store::navigation::query::{
    attention_navigation_documents, document_matches_terms, filter_navigation_search_documents,
    navigation_search_terms,
};
use roost_client_core::store::navigation::{
    NavigationSearchAttention, NavigationSearchDocument, attention_for_level_token, session_href,
};
use roost_client_core::store::palette::matches_query;

fn document(
    session_id: &str,
    available: bool,
    attention: Option<NavigationSearchAttention>,
    unseen: bool,
    arrival: u64,
) -> NavigationSearchDocument {
    let level = match attention {
        Some(NavigationSearchAttention::Blocked) => "blocked",
        Some(NavigationSearchAttention::Done) => "done",
        None => "working",
    };
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
        search_text: format!("{session_id} {level}"),
        activity_at: arrival as i64,
        available,
        agent_status: Some(level.to_owned()),
        agent_attention: attention,
        agent_unseen: unseen,
        agent_id: None,
        agent_message: None,
        agent_updated_at_ms: None,
        agent_arrival: arrival,
        client_only: false,
    }
}

#[test]
fn every_agent_level_token_maps_onto_exactly_one_attention_state() {
    // The five tokens the agent-status owner emits, and nothing else.
    assert_eq!(
        attention_for_level_token("blocked"),
        Some(NavigationSearchAttention::Blocked)
    );
    assert_eq!(
        attention_for_level_token("done"),
        Some(NavigationSearchAttention::Done)
    );
    assert_eq!(attention_for_level_token("working"), None);
    assert_eq!(attention_for_level_token("idle"), None);
    assert_eq!(attention_for_level_token("unknown"), None);
    assert_eq!(attention_for_level_token(""), None);
    assert_eq!(
        attention_for_level_token("Blocked"),
        None,
        "the vocabulary is frozen lowercase"
    );
}

#[test]
fn every_query_term_must_match_and_may_match_different_fields() {
    let rows = vec![document("api gateway", true, None, false, 1)];
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

#[test]
fn a_blocked_row_outranks_a_done_one_and_an_unreachable_row_outranks_nothing() {
    let rows = vec![
        document(
            "d-done",
            true,
            Some(NavigationSearchAttention::Done),
            true,
            9,
        ),
        document(
            "c-blocked",
            true,
            Some(NavigationSearchAttention::Blocked),
            true,
            1,
        ),
        document(
            "b-unreachable",
            false,
            Some(NavigationSearchAttention::Blocked),
            true,
            99,
        ),
        document("a-working", true, None, false, 99),
    ];
    let order: Vec<&str> = attention_navigation_documents(&rows)
        .iter()
        .map(|row| row.session_id.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["c-blocked", "d-done", "b-unreachable"],
        "reachable first whatever the level, then blocked before done, then unseen, then recency"
    );
}

#[test]
fn an_unseen_status_outranks_a_seen_one_at_the_same_level() {
    let rows = vec![
        document(
            "seen",
            true,
            Some(NavigationSearchAttention::Blocked),
            false,
            99,
        ),
        document(
            "unseen",
            true,
            Some(NavigationSearchAttention::Blocked),
            true,
            1,
        ),
    ];
    let order: Vec<&str> = attention_navigation_documents(&rows)
        .iter()
        .map(|row| row.session_id.as_str())
        .collect();
    assert_eq!(
        order,
        vec!["unseen", "seen"],
        "recency never outranks what is unseen"
    );
}
