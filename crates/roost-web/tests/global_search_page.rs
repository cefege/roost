//! The `/search` page's reader-visible contract, native because every rule here
//! is a pure function over the route or over the coordinator's answer: what the
//! address bar says, which rows the page keeps, and what it tells the reader
//! when the fleet could not be searched completely.
//!
//! The surface this page replaced was `not_served::NotServed`, which named the
//! path instead of searching it — so the tests below are the page existing at
//! all, not refinements of it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::global_search::GlobalSearchResults;
use roost_client_core::search::global::{
    GlobalSearchMatch, GlobalSearchPartial, GlobalSearchPartialReason,
};
use roost_protocol::wire::SessionId;
use roost_web::components::global_search::content::{
    incomplete, join_matches, partial_line, summary,
};
use roost_web::components::global_search::query::{SearchRouteQuery, SearchScope};
use roost_web::routes::percent_encode;

const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
const OTHER_SESSION: &str = "00000000-0000-4000-8000-00000000000b";

/// A fixture session id. Only a value that is not a uuid is rejected, so the
/// constants above are the thing this can catch.
fn session_id(value: &str) -> SessionId {
    SessionId::try_from(value.to_owned()).expect("a fixture session id")
}

fn match_in(session: &str, row: u64) -> GlobalSearchMatch {
    GlobalSearchMatch {
        session_id: session_id(session),
        row,
        col: 0,
        len: 6,
        preview: "global-404ef456-primary".to_owned(),
        grid_epoch: "epoch-1".to_owned(),
    }
}

#[test]
fn a_query_written_in_the_address_bar_reads_back_as_the_same_search() {
    let query = SearchRouteQuery {
        scope: SearchScope::All,
        text: "global-404ef456".to_owned(),
        case_sensitive: true,
    };
    assert_eq!(SearchRouteQuery::parse(&query.to_path()), query);
}

#[test]
fn a_query_that_would_read_as_two_parameters_stays_one_parameter() {
    let query = SearchRouteQuery::default().with_text("a&b=c");
    assert_eq!(query.to_path(), "/search?q=a%26b%3Dc");
    assert_eq!(SearchRouteQuery::parse(&query.to_path()).text, "a&b=c");
    assert_eq!(percent_encode("a&b=c"), "a%26b%3Dc");
}

#[test]
fn the_attention_filter_searches_no_terminal_content() {
    let attention = SearchRouteQuery {
        scope: SearchScope::Attention,
        text: "blocked".to_owned(),
        case_sensitive: true,
    };
    assert!(attention.content_query().is_empty());
    assert_eq!(attention.to_path(), "/search?scope=attention&q=blocked");
}

#[test]
fn a_content_match_for_a_session_this_client_cannot_see_is_still_listed() {
    // The coordinator searched a machine this browser is not following. Hiding
    // the row would report the fleet as having fewer matches than it has.
    let rows = join_matches(&[match_in(SESSION, 7)], &[]);
    assert_eq!(rows.len(), 1);
    assert!(rows[0].document.is_none());
    assert!(incomplete(&GlobalSearchResults::default(), 1));
}

#[test]
fn one_page_over_every_eligible_session_reads_as_complete() {
    let results = GlobalSearchResults {
        searched_sessions: 2,
        eligible_sessions: 2,
        has_searched: true,
        ..GlobalSearchResults::default()
    };
    assert!(!incomplete(&results, 0));
    assert_eq!(
        summary(&results, &[], false, "global-404ef456"),
        "0 matches across all 2 sessions searched"
    );
}

#[test]
fn the_headline_counts_rows_the_projection_knows_and_the_incomplete_panel_owns_the_rest() {
    // v2 joins the coordinator's rows through the navigation projection and
    // counts what survived the join; the rows that did not are reported beside
    // it as missing coverage rather than folded into the headline.
    let results = GlobalSearchResults {
        matches: vec![match_in(SESSION, 7)],
        searched_sessions: 1,
        eligible_sessions: 1,
        has_searched: true,
        ..GlobalSearchResults::default()
    };
    let rows = join_matches(&results.matches, &[]);
    assert!(!incomplete(&results, 0));
    assert_eq!(
        summary(&results, &rows, false, "global-404ef456"),
        "0 matches across all 1 sessions searched"
    );
    assert!(incomplete(&results, 1));
}

#[test]
fn a_cursor_or_unsearched_sessions_is_told_rather_than_hidden() {
    let with_cursor = GlobalSearchResults {
        matches: vec![match_in(SESSION, 7)],
        next_cursor: Some("cursor-2".to_owned()),
        searched_sessions: 1,
        eligible_sessions: 2,
        has_searched: true,
        ..GlobalSearchResults::default()
    };
    let rows = join_matches(&with_cursor.matches, &[]);
    assert!(incomplete(&with_cursor, 0));
    assert_eq!(
        summary(&with_cursor, &rows, false, "global-404ef456"),
        "0 matches across 1 of 2 sessions — coverage is partial"
    );
}

#[test]
fn an_unreachable_machine_is_reported_as_a_partial_not_as_an_empty_fleet() {
    let results = GlobalSearchResults {
        partials: vec![GlobalSearchPartial {
            session_id: session_id(OTHER_SESSION),
            reason: GlobalSearchPartialReason::WorkerUnavailable,
        }],
        searched_sessions: 1,
        eligible_sessions: 2,
        has_searched: true,
        ..GlobalSearchResults::default()
    };
    assert!(incomplete(&results, 0));
    assert_eq!(
        summary(&results, &[], false, "global-404ef456"),
        "0 matches across 1 of 2 sessions — coverage is partial"
    );
}

#[test]
fn a_session_the_scan_could_not_finish_is_named_with_its_reason() {
    let partial = GlobalSearchPartial {
        session_id: session_id(OTHER_SESSION),
        reason: GlobalSearchPartialReason::Deadline,
    };
    assert_eq!(
        partial_line(&partial, &[]),
        "A session no longer in the current view did not finish before the page deadline."
    );
}
