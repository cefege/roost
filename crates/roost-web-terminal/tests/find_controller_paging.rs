//! Terminal-find page-chain tests: exclusive cursor progression across pages,
//! capped partial states, paging back past the match cap, malformed-range and
//! nonprogressing-cursor failures, and query cutover with cancellation. Test
//! names are v2's, from `apps/web/tests/terminalFindController-paging.test.ts`.

mod find_support;

use std::collections::BTreeSet;

use find_support::{EPOCH_A, FindHarness, RpcAnswer, SEED, SESSION, Shape, reply};
use roost_protocol::terminal_search::{
    TERMINAL_SEARCH_MAX_MATCHES, TERMINAL_SEARCH_MAX_PAGES, TERMINAL_SEARCH_MAX_ROWS,
};
use roost_web_terminal::find::SearchStop;

const MAX_MATCHES: usize = TERMINAL_SEARCH_MAX_MATCHES as usize;

fn row_limit(start: u32, end: u32, next: u32) -> Shape {
    let stop = SearchStop::RowLimit;
    Shape {
        stop,
        start,
        end,
        next: Some(next),
    }
}

fn match_limit(start: u32, end: u32, next: u32) -> Shape {
    let stop = SearchStop::MatchLimit;
    Shape {
        stop,
        start,
        end,
        next: Some(next),
    }
}

#[test]
fn chains_sparse_pages_sequentially_with_exclusive_non_overlapping_cursors() {
    let mut h = FindHarness::new();
    h.set_rpc(|request, _| {
        RpcAnswer::Reply(match request.before_row {
            None => reply(&[1900], EPOCH_A, row_limit(1200, 2000, 1200)),
            Some(1200) => reply(&[], EPOCH_A, row_limit(400, 1200, 400)),
            Some(_) => reply(
                &[50],
                EPOCH_A,
                Shape {
                    start: 0,
                    end: 400,
                    ..Shape::default()
                },
            ),
        })
    });
    h.set_query("sparse");
    h.fire_debounce();
    assert_eq!(h.max_in_flight, 1);
    let before: Vec<Option<u32>> = h.requests.iter().map(|r| r.before_row).collect();
    assert_eq!(before, vec![None, Some(1200), Some(400)]);
    let rows: Vec<u32> = h.requests.iter().map(|r| r.max_rows).collect();
    assert_eq!(rows, vec![TERMINAL_SEARCH_MAX_ROWS; 3]);
    let caps: Vec<u32> = h.requests.iter().map(|r| r.max_matches).collect();
    let max = TERMINAL_SEARCH_MAX_MATCHES;
    assert_eq!(caps, vec![max, max - 1, max - 1]);
    assert_eq!(h.rows(), vec![50, 1900]);
    assert_eq!(h.index(), 2);
    assert!(!h.find.publication().is_truncated());
    assert!(!h.find.publication().has_failed());
}

#[test]
fn match_limit_and_deadline_publish_explicit_capped_partial_states() {
    let mut capped = FindHarness::new();
    let newest_first: Vec<u32> = (0..TERMINAL_SEARCH_MAX_MATCHES)
        .map(|idx| 1999 - idx)
        .collect();
    let stop = SearchStop::MatchLimit;
    capped.set_rpc(move |_, _| {
        RpcAnswer::Reply(reply(
            &newest_first,
            EPOCH_A,
            Shape {
                stop,
                ..Shape::default()
            },
        ))
    });
    capped.set_query("many");
    capped.fire_debounce();
    assert_eq!(capped.find.publication().matches().len(), MAX_MATCHES);
    assert!(capped.find.publication().is_truncated());
    assert!(!capped.find.publication().has_failed());

    let mut timed = FindHarness::new();
    let stop = SearchStop::Deadline;
    timed.set_rpc(move |_, _| {
        RpcAnswer::Reply(reply(
            &[700],
            EPOCH_A,
            Shape {
                stop,
                ..Shape::default()
            },
        ))
    });
    timed.set_query("slow");
    timed.fire_debounce();
    assert_eq!(timed.rows(), vec![700]);
    assert!(timed.find.publication().is_truncated());
    assert!(timed.find.publication().has_failed());

    let mut empty_timed = FindHarness::new();
    let shape = Shape {
        stop,
        start: 0,
        end: 0,
        next: None,
    };
    empty_timed.set_rpc(move |_, _| RpcAnswer::Reply(reply(&[], EPOCH_A, shape)));
    empty_timed.set_query("too slow");
    empty_timed.fire_debounce();
    assert!(empty_timed.find.publication().matches().is_empty());
    assert!(empty_timed.find.publication().is_truncated());
    assert!(empty_timed.find.publication().has_failed());
}

fn full_page(newest_row: u32) -> Vec<u32> {
    (0..TERMINAL_SEARCH_MAX_MATCHES)
        .map(|idx| newest_row - idx)
        .collect()
}

fn step_back_onto_oldest(h: &mut FindHarness) {
    while h.index() > 1 {
        h.step(-1);
    }
    h.step(-1);
}

fn distinct(rows: &[u32]) -> usize {
    rows.iter().collect::<BTreeSet<_>>().len()
}

#[test]
fn stepping_back_past_the_oldest_match_pages_beyond_the_match_cap() {
    let mut h = FindHarness::new();
    h.set_rpc(|request, _| {
        RpcAnswer::Reply(match request.before_row {
            None => reply(&full_page(1999), EPOCH_A, match_limit(1744, 2000, 1744)),
            Some(1744) => reply(&full_page(1743), EPOCH_A, match_limit(1488, 1744, 1488)),
            Some(_) => reply(
                &[1000],
                EPOCH_A,
                Shape {
                    start: 0,
                    end: 1488,
                    ..Shape::default()
                },
            ),
        })
    });
    h.set_query("many");
    h.fire_debounce();
    assert_eq!(h.find.publication().matches().len(), MAX_MATCHES);
    assert!(h.find.publication().is_truncated());
    assert!(!h.find.publication().has_failed());

    step_back_onto_oldest(&mut h);
    assert_eq!(h.requests.len(), 2);
    assert_eq!(h.requests[1].before_row, Some(1744));
    assert_eq!(h.requests[1].max_matches, TERMINAL_SEARCH_MAX_MATCHES);
    let two_pages = h.rows();
    assert_eq!(two_pages.len(), MAX_MATCHES * 2);
    assert_eq!(distinct(&two_pages), two_pages.len());
    assert!(
        two_pages[..MAX_MATCHES]
            .iter()
            .max()
            .is_some_and(|row| *row < 1744)
    );
    // The newest row of the page just paged in becomes active and is revealed.
    let active = h.index() as usize - 1;
    assert_eq!(h.find.publication().matches()[active].row, 1743);
    assert_eq!(h.host.jumps().last(), Some(&1743));
    assert!(h.find.publication().is_truncated());

    step_back_onto_oldest(&mut h);
    assert_eq!(h.requests.len(), 3);
    assert_eq!(h.requests[2].before_row, Some(1488));
    let all_rows = h.rows();
    assert_eq!(all_rows.len(), MAX_MATCHES * 2 + 1);
    assert_eq!(distinct(&all_rows), all_rows.len());
    let mut sorted = all_rows.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, all_rows);
    assert!(!h.find.publication().is_truncated());
    assert!(!h.find.publication().has_failed());

    // The last page completed, so stepping off the oldest match wraps instead
    // of asking for a page that cannot exist.
    step_back_onto_oldest(&mut h);
    assert_eq!(h.requests.len(), 3);
    assert_eq!(h.index() as usize, all_rows.len());
}

#[test]
fn malformed_ranges_and_nonprogressing_cursors_fail_without_another_page() {
    let mut stuck = FindHarness::new();
    stuck.set_rpc(|_, _| RpcAnswer::Reply(reply(&[], EPOCH_A, row_limit(1000, 2000, 2000))));
    stuck.set_query("stuck");
    stuck.fire_debounce();
    assert_eq!(stuck.requests.len(), 1);
    assert!(stuck.find.publication().has_failed());
    stuck.dispose();

    let mut overlap = FindHarness::new();
    let mut call = 0;
    overlap.set_rpc(move |_, _| {
        call += 1;
        RpcAnswer::Reply(if call == 1 {
            reply(&[1500], EPOCH_A, row_limit(1000, 2000, 1000))
        } else {
            reply(
                &[500],
                EPOCH_A,
                Shape {
                    start: 0,
                    end: 1500,
                    ..Shape::default()
                },
            )
        })
    });
    overlap.set_query("overlap");
    overlap.fire_debounce();
    assert_eq!(overlap.requests.len(), 2);
    assert_eq!(overlap.rows(), vec![1500]);
    assert!(overlap.find.publication().has_failed());

    let mut bounded = FindHarness::new();
    bounded.set_rpc(|request, _| {
        let end = request.before_row.unwrap_or(2000);
        RpcAnswer::Reply(reply(&[], EPOCH_A, row_limit(end - 1, end, end - 1)))
    });
    bounded.set_query("bounded chain");
    bounded.fire_debounce();
    assert_eq!(bounded.requests.len(), TERMINAL_SEARCH_MAX_PAGES as usize);
    assert!(bounded.find.publication().has_failed());
}

#[test]
fn a_new_query_aborts_the_old_chain_its_stale_page_cannot_publish_or_continue() {
    let mut h = FindHarness::new();
    h.set_rpc(|request, _| match request.query.as_str() {
        "old" => RpcAnswer::Hold,
        _ => RpcAnswer::Reply(reply(&[80], EPOCH_A, Shape::default())),
    });
    h.set_query("old");
    h.fire_debounce();
    h.set_query("new");
    let old_id = h.requests[0].search_id.clone();
    assert_eq!(h.cancellations, vec![old_id.clone()]);
    h.fire_debounce();
    assert_eq!(h.rows(), vec![80]);
    h.resolve_held(
        &old_id,
        reply(&[1500], EPOCH_A, row_limit(1000, 2000, 1000)),
    );
    let queries: Vec<&str> = h.requests.iter().map(|r| r.query.as_str()).collect();
    assert_eq!(queries, vec!["old", "new"]);
    assert_eq!(h.rows(), vec![80]);
    assert_ne!(h.requests[1].search_id, old_id);
}

#[test]
fn a_stale_page_landing_while_the_new_chain_is_in_flight_is_dropped() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Hold);
    h.set_query("old");
    h.fire_debounce();
    h.set_query("new");
    h.fire_debounce();
    let old_id = h.requests[0].search_id.clone();
    let new_id = h.requests[1].search_id.clone();
    h.resolve_held(
        &old_id,
        reply(&[1500], EPOCH_A, row_limit(1000, 2000, 1000)),
    );
    assert_eq!(h.requests.len(), 2);
    assert!(h.find.publication().matches().is_empty());
    h.resolve_held(&new_id, reply(&[80], EPOCH_A, Shape::default()));
    assert_eq!(h.rows(), vec![80]);
}

#[test]
fn regex_and_case_flags_survive_the_paged_request_cutover() {
    let mut h = FindHarness::new();
    h.set_query("a.*b");
    h.toggle_regex();
    h.toggle_case_sensitive();
    h.fire_debounce();
    assert_eq!(h.requests.len(), 1);
    let request = &h.requests[0];
    assert_eq!(request.session_id, SESSION);
    assert_eq!(request.grid_epoch, EPOCH_A);
    assert_eq!(request.query, "a.*b");
    assert!(request.case_sensitive);
    assert!(request.regex);
    assert_eq!(request.max_rows, TERMINAL_SEARCH_MAX_ROWS);
    assert_eq!(request.max_matches, TERMINAL_SEARCH_MAX_MATCHES);
    assert!(request.search_id.starts_with(SEED));
}
