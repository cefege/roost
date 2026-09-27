//! Global search is a QUERY AGAINST THE COORDINATOR'S LEDGER, and these tests
//! hold it to that.
//!
//! The failure they exist to prevent is a client that indexes the machines it
//! happens to be connected to and reports "no results" for the rest of the fleet
//! as though the fleet had none. So: the request carries no machine, the
//! coordinator's own session count is published rather than recomputed, rows
//! from three machines survive one merge, and a superseded query's page cannot
//! land in this query's list.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_client_core::client::global_search::{
    GLOBAL_SEARCH_DEBOUNCE_MS, GlobalSearchController, GlobalSearchQuery, GlobalSearchRequest,
    SetSearchOutcome, retain_joinable_matches,
};
use roost_client_core::search::global::{
    GlobalSearchMatch, GlobalSearchPartial, GlobalSearchPartialReason, GlobalSearchResponse,
};
use roost_protocol::terminal_search::{
    GLOBAL_TERMINAL_SEARCH_MAX_MATCHES, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS,
};
use roost_protocol::wire::SessionId;

const SESSION_ON_A: &str = "10000000-0000-4000-8000-00000000000a";
const SESSION_ON_B: &str = "20000000-0000-4000-8000-00000000000b";
const SESSION_ON_C: &str = "30000000-0000-4000-8000-00000000000c";

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
fn the_first_page_is_debounced_and_sends_the_coordinators_own_limits() {
    let mut controller = GlobalSearchController::new();
    assert_eq!(
        controller.set_search(
            GlobalSearchQuery {
                query: "needle".to_owned(),
                case_sensitive: true,
            },
            1_000,
        ),
        SetSearchOutcome::Debouncing
    );
    assert!(
        controller.take_first_page("search-1", 3, 1_000).is_none(),
        "a query sent before its debounce ends is one fleet scan per keystroke"
    );
    let request = controller
        .take_first_page("search-1", 3, 1_000 + GLOBAL_SEARCH_DEBOUNCE_MS)
        .expect("the debounce has ended");
    assert_eq!(request.query, "needle");
    assert!(request.case_sensitive);
    assert_eq!(request.cursor, None);
    assert_eq!(request.call_id, 3);
    assert_eq!(
        request.max_sessions, GLOBAL_TERMINAL_SEARCH_MAX_SESSIONS as u32,
        "the cap is the coordinator's, because the coordinator is what does the \
         scanning"
    );
    assert_eq!(request.max_matches, GLOBAL_TERMINAL_SEARCH_MAX_MATCHES);
    assert_eq!(request.search_id, "search-1");

    assert!(
        controller.take_first_page("search-2", 8, 9_000).is_none(),
        "a second page may not be issued while one is outstanding: the cursor is \
         a single continuation and two readers of one scan interleave rows"
    );
}

#[test]
fn results_from_three_machines_survive_one_merge_and_the_fleet_count_is_published_verbatim() {
    let (mut controller, first) = started("needle");
    assert!(
        controller.accept_page(
            first.call_id,
            &first.search_id,
            &page(
                vec![
                    match_in(SESSION_ON_A, 10, 2, 6, "epoch-a"),
                    match_in(SESSION_ON_B, 4, 0, 6, "epoch-b"),
                ],
                vec![GlobalSearchPartial {
                    session_id: session_id(SESSION_ON_C),
                    reason: GlobalSearchPartialReason::WorkerUnavailable,
                }],
                Some("cursor-2"),
                2,
                9,
            ),
        ),
        "the first page publishes"
    );
    let results = controller.results();
    assert_eq!(
        results.matches.len(),
        2,
        "rows from both machines are published"
    );
    assert_eq!(results.searched_sessions, 2);
    assert_eq!(
        results.eligible_sessions, 9,
        "the coordinator counted nine eligible sessions across the fleet, and \
         that number is published as it arrived rather than recomputed from the \
         sessions this browser happens to hold"
    );
    assert!(results.truncated);
    assert_eq!(results.partials.len(), 1);

    let more = controller.load_more(2).expect("a cursor stands");
    assert_eq!(more.cursor.as_deref(), Some("cursor-2"));
    assert!(controller.accept_page(
        more.call_id,
        &more.search_id,
        &page(
            vec![match_in(SESSION_ON_C, 7, 1, 6, "epoch-c")],
            vec![],
            None,
            9,
            9,
        ),
    ));
    let results = controller.results();
    assert_eq!(
        results.matches.len(),
        3,
        "the third machine's row joined the list: a search that answered for \
         only the machines this browser is connected to would have published two"
    );
    assert_eq!(results.searched_sessions, 9);
    assert_eq!(results.next_cursor, None);
    assert!(
        !results.truncated,
        "the coordinator stopped for lack of rows, not for a cap"
    );
    assert!(
        results.partials.is_empty(),
        "the retryable worker-unavailable partial was replaced by the \
         continuation that searched that session successfully"
    );
}

#[test]
fn a_session_whose_grid_was_replaced_under_the_scan_loses_its_own_rows() {
    let (mut controller, first) = started("needle");
    controller.accept_page(
        first.call_id,
        &first.search_id,
        &page(
            vec![
                match_in(SESSION_ON_A, 10, 0, 6, "epoch-a1"),
                match_in(SESSION_ON_B, 4, 0, 6, "epoch-b1"),
            ],
            vec![],
            Some("cursor-2"),
            2,
            2,
        ),
    );
    let more = controller.load_more(2).expect("a cursor stands");
    assert!(controller.accept_page(
        more.call_id,
        &more.search_id,
        &page(
            vec![
                // Read from the grid that replaced A's: not comparable with the
                // row published above it.
                match_in(SESSION_ON_A, 12, 3, 6, "epoch-a2"),
                match_in(SESSION_ON_B, 8, 0, 6, "epoch-b1"),
            ],
            vec![GlobalSearchPartial {
                session_id: session_id(SESSION_ON_A),
                reason: GlobalSearchPartialReason::EpochChanged,
            }],
            None,
            2,
            2,
        ),
    ));
    let matches = &controller.results().matches;
    assert!(
        !matches
            .iter()
            .any(|row| row.session_id == session_id(SESSION_ON_A)),
        "a retired epoch's rows are withdrawn, not merged alongside the new ones: \
         two rows from two grids in one list is a result that points at the \
         wrong line"
    );
    assert_eq!(
        matches
            .iter()
            .filter(|row| row.session_id == session_id(SESSION_ON_B))
            .count(),
        2
    );
    assert!(!controller.results().partials.is_empty());
}

#[test]
fn a_match_whose_session_the_client_no_longer_holds_is_not_offered() {
    let rows = vec![
        match_in(SESSION_ON_A, 1, 0, 6, "epoch-a"),
        match_in(SESSION_ON_B, 2, 0, 6, "epoch-b"),
    ];
    let known: BTreeSet<SessionId> = [session_id(SESSION_ON_A)].into_iter().collect();
    let joinable = retain_joinable_matches(rows, &known);
    assert_eq!(joinable.len(), 1);
    assert_eq!(
        joinable[0].session_id,
        session_id(SESSION_ON_A),
        "a row whose session is gone has nowhere to navigate to"
    );
}
