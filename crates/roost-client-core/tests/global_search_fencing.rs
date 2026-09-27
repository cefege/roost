//! The fences on a global search: the version a superseded page cannot publish
//! through, the `search_id` the coordinator can cancel, the retry that re-arms
//! the debounce, and the credential boundary that takes the results with it.
//!
//! Split from `global_search_across_machines.rs` by subject, not by size: this
//! file is about which page may write, that one is about what the coordinator
//! said.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::global_search::{
    GLOBAL_SEARCH_DEBOUNCE_MS, GlobalSearchController, GlobalSearchQuery, GlobalSearchRequest,
    SetSearchOutcome,
};
use roost_client_core::search::global::{
    GlobalSearchMatch, GlobalSearchPartial, GlobalSearchResponse,
};
use roost_protocol::terminal_search::TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS;
use roost_protocol::wire::SessionId;

const SESSION_ON_A: &str = "10000000-0000-4000-8000-00000000000a";
const SESSION_ON_B: &str = "20000000-0000-4000-8000-00000000000b";

fn session_id(value: &str) -> SessionId {
    SessionId::try_from(value).expect("a session id")
}

fn match_in(session: &str, row: u64, col: u32, len: u32, epoch: &str) -> GlobalSearchMatch {
    GlobalSearchMatch {
        session_id: session_id(session),
        row,
        col,
        len,
        preview: format!("{row}: needle"),
        grid_epoch: epoch.to_owned(),
    }
}

fn page(
    matches: Vec<GlobalSearchMatch>,
    partials: Vec<GlobalSearchPartial>,
    next_cursor: Option<&str>,
    searched: u32,
    eligible: u32,
) -> GlobalSearchResponse {
    GlobalSearchResponse {
        matches,
        partials,
        next_cursor: next_cursor.map(str::to_owned),
        searched_sessions: searched,
        eligible_sessions: eligible,
        truncated: next_cursor.is_some(),
    }
}

fn query(text: &str) -> GlobalSearchQuery {
    GlobalSearchQuery {
        query: text.to_owned(),
        case_sensitive: false,
    }
}

/// A controller whose first page is issued and outstanding, with the request
/// that issued it, so a test can answer THAT page rather than a fresh one.
fn started(text: &str) -> (GlobalSearchController, GlobalSearchRequest) {
    let mut controller = GlobalSearchController::new();
    assert_eq!(
        controller.set_search(query(text), 1_000),
        SetSearchOutcome::Debouncing
    );
    let request = controller
        .take_first_page("search-1", 7, 1_000 + GLOBAL_SEARCH_DEBOUNCE_MS)
        .expect("the debounce has ended");
    (controller, request)
}

#[test]
fn a_page_from_a_superseded_query_cannot_land_in_this_querys_list() {
    let (mut controller, stale) = started("first");
    controller.set_search(query("second"), 10_000);
    let current = controller
        .take_first_page("search-2", 9, 10_000 + GLOBAL_SEARCH_DEBOUNCE_MS)
        .expect("the second query is armed");

    // The FIRST query's page answers late, under its own identity.
    assert!(
        !controller.accept_page(
            stale.call_id,
            &stale.search_id,
            &page(
                vec![match_in(SESSION_ON_A, 1, 0, 6, "epoch-a")],
                vec![],
                None,
                1,
                1,
            ),
        ),
        "a page from a superseded query must be dropped whole, not merged"
    );
    assert!(controller.results().matches.is_empty());
    assert!(!controller.results().has_searched);

    assert!(controller.accept_page(
        current.call_id,
        &current.search_id,
        &page(
            vec![match_in(SESSION_ON_B, 2, 0, 6, "epoch-b")],
            vec![],
            None,
            1,
            1,
        ),
    ));
    assert_eq!(controller.results().matches.len(), 1);
    assert_eq!(
        controller.results().matches[0].session_id,
        session_id(SESSION_ON_B),
        "only the current query's row is in the list"
    );
}

#[test]
fn abandoning_a_search_owes_the_coordinator_a_cancel_and_keeps_the_rows_already_read() {
    let (mut controller, first) = started("needle");
    controller.accept_page(
        first.call_id,
        &first.search_id,
        &page(
            vec![match_in(SESSION_ON_A, 3, 0, 6, "epoch-a")],
            vec![],
            Some("cursor-2"),
            1,
            1,
        ),
    );
    assert!(controller.active_search_id().is_some());

    let cancel = controller
        .stop_logical_search()
        .expect("a running search must hand back the identity to cancel");
    assert_eq!(
        cancel, "search-1",
        "the coordinator's ledger keeps a scan running for a cursor's whole \
         lifetime, so an abandoned search that is not cancelled is work nobody \
         is waiting for"
    );
    assert_eq!(controller.active_search_id(), None);
    assert!(
        !controller.is_loading(),
        "an abandoned search must not leave a page outstanding"
    );
    assert_eq!(
        controller.results().matches.len(),
        1,
        "an answer the coordinator already gave is still a true answer, and a \
         viewer who keeps typing should not watch the list empty per keystroke"
    );
}

#[test]
fn a_failed_page_is_retryable_and_a_page_from_another_search_does_not_publish_its_failure() {
    let (mut controller, stale) = started("needle");
    controller.set_search(query("other"), 10_000);
    let current = controller
        .take_first_page("search-2", 9, 10_000 + GLOBAL_SEARCH_DEBOUNCE_MS)
        .expect("the second query is armed");

    assert!(!controller.fail_page(stale.call_id, "gateway timeout".to_owned()));
    assert_eq!(controller.results().error, None);

    assert!(controller.fail_page(current.call_id, "gateway timeout".to_owned()));
    assert_eq!(
        controller.results().error.as_deref(),
        Some("gateway timeout")
    );
    assert!(controller.results().retryable);

    assert_eq!(controller.retry(20_000), SetSearchOutcome::Debouncing);
    let again = controller
        .take_first_page("search-3", 4, 20_000 + GLOBAL_SEARCH_DEBOUNCE_MS)
        .expect("a retry re-arms the same debounce a fresh keystroke would");
    assert!(controller.accept_page(
        again.call_id,
        &again.search_id,
        &page(vec![], vec![], None, 0, 0),
    ));
    assert_eq!(
        controller.results().error,
        None,
        "an answer clears the failure"
    );
}

#[test]
fn a_query_longer_than_the_shared_limit_is_refused_without_a_request() {
    let mut controller = GlobalSearchController::new();
    let long = "x".repeat(TERMINAL_SEARCH_QUERY_MAX_CODE_POINTS + 1);
    assert_eq!(
        controller.set_search(query(&long), 0),
        SetSearchOutcome::TooLong
    );
    assert!(controller.results().error.is_some());
    assert!(controller.results().has_searched);
    assert!(
        controller
            .take_first_page("search-1", 1, GLOBAL_SEARCH_DEBOUNCE_MS)
            .is_none(),
        "a refused query must never reach the coordinator"
    );

    assert_eq!(
        controller.set_search(query("   "), 0),
        SetSearchOutcome::Cleared,
        "whitespace is an empty query, not a search for spaces"
    );
}

#[test]
fn a_credential_boundary_clears_the_results_and_restores_the_query_afterwards() {
    let (mut controller, first) = started("needle");
    controller.accept_page(
        first.call_id,
        &first.search_id,
        &page(
            vec![match_in(SESSION_ON_A, 3, 0, 6, "epoch-a")],
            vec![],
            None,
            1,
            1,
        ),
    );
    let cancel = controller
        .reset_for_auth_boundary()
        .expect("the running search owes a cancel");
    assert_eq!(cancel, "search-1");
    assert!(
        controller.results().matches.is_empty(),
        "rows read under a revoked credential are content, and content goes"
    );
    assert!(controller.is_suspended());

    assert_eq!(
        controller.set_search(query("other"), 5_000),
        SetSearchOutcome::Unchanged,
        "typing while suspended must not start a scan under a credential that \
         has not been resolved yet"
    );
    assert_eq!(controller.desired().query, "other");

    assert_eq!(
        controller.resume_after_auth_boundary(6_000),
        SetSearchOutcome::Debouncing
    );
    assert!(!controller.is_suspended());
    assert_eq!(
        controller.desired().query,
        "other",
        "the restored search is the one the viewer last asked for"
    );
}
