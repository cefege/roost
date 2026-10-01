//! Terminal-find epoch-seed tests: a fresh chain pins NO epoch and lets its
//! first answer name one, an adopted chain refuses to publish on a grid the
//! page was not read against, a resumption keeps the epoch its cursor was
//! taken on, and the epoch retry stays bounded at one.

mod find_support;

use find_support::{EPOCH_A, EPOCH_B, FindHarness, RpcAnswer, SESSION, Shape, reply};
use roost_web_terminal::find::{ChainOutcome, ChainStep, FindChain, OlderMatchPage, SearchStop};

fn epochs(h: &FindHarness) -> Vec<String> {
    h.requests
        .iter()
        .map(|request| request.grid_epoch.clone())
        .collect()
}

/// Every highlight list the grid was told, in order.
fn painted_rows(h: &FindHarness) -> Vec<Vec<u32>> {
    h.host
        .published()
        .into_iter()
        .map(|published| published.rows)
        .collect()
}

/// The epoch a finished chain fenced its match under, if it read one.
fn adopted_epoch(step: ChainStep) -> Option<String> {
    match step {
        ChainStep::Finish(ChainOutcome::Matches { matches, .. }) => {
            matches.first().map(|found| found.epoch.clone())
        }
        ChainStep::Finish(_) | ChainStep::Issue(_) => None,
    }
}

#[test]
fn a_fresh_chain_asks_for_no_epoch_and_adopts_the_first_answer() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, host| {
        // The pane is behind when the request goes out and the full frame
        // naming the current grid lands while the page is in flight.
        host.anchor.grid_epoch = EPOCH_B.to_string();
        RpcAnswer::Reply(reply(&[400], EPOCH_B, Shape::default()))
    });
    h.set_query("needle");
    h.fire_debounce();
    assert_eq!(epochs(&h), vec![String::new()]);
    assert_eq!(h.rows_with_epoch(), vec![(400, EPOCH_B.to_string())]);
    assert_eq!(h.index(), 1);
    assert_eq!(h.pulled, vec![400]);
    assert_eq!(h.host.jumps(), vec![400]);
    assert!(!h.find.publication().has_failed());
}

#[test]
fn the_epoch_retry_re_reads_the_query_once_the_pane_catches_up() {
    let mut h = FindHarness::new();
    let mut call = 0;
    h.set_rpc(move |_, host| {
        call += 1;
        if call > 1 {
            host.anchor.grid_epoch = EPOCH_B.to_string();
        }
        RpcAnswer::Reply(reply(&[400], EPOCH_B, Shape::default()))
    });
    h.set_query("needle");
    h.fire_debounce();
    assert_eq!(epochs(&h), vec![String::new(), String::new()]);
    assert_eq!(h.rows_with_epoch(), vec![(400, EPOCH_B.to_string())]);
    assert_eq!(h.host.jumps(), vec![400]);
    assert!(!h.find.publication().has_failed());
}

#[test]
fn an_adopted_epoch_never_paints_on_a_grid_it_was_not_read_against() {
    let mut h = FindHarness::new();
    let mut call = 0;
    h.set_rpc(move |_, _| {
        call += 1;
        let epoch = if call == 1 { EPOCH_B } else { "grid-c:0" };
        RpcAnswer::Reply(reply(&[400], epoch, Shape::default()))
    });
    h.set_query("needle");
    h.fire_debounce();
    assert!(h.find.publication().matches().is_empty());
    assert!(h.find.publication().has_failed());
    assert!(h.host.jumps().is_empty());
    // No highlight list the grid was told ever named the match row.
    assert!(painted_rows(&h).iter().all(|rows| rows.is_empty()));
}

#[test]
fn a_resumed_page_carries_and_enforces_the_epoch_its_cursor_was_taken_on() {
    let mut h = FindHarness::new();
    let capped = Shape {
        stop: SearchStop::MatchLimit,
        start: 1744,
        end: 2000,
        next: Some(1744),
    };
    let mut call = 0;
    h.set_rpc(move |_, host| {
        call += 1;
        match call {
            1 => RpcAnswer::Reply(reply(&[1900], EPOCH_A, capped)),
            2 => {
                // The pane renumbers under the resumed page, which still names
                // the epoch its cursor was taken on.
                host.anchor.grid_epoch = EPOCH_B.to_string();
                RpcAnswer::Reply(reply(&[1500], EPOCH_A, Shape::default()))
            }
            _ => RpcAnswer::Reply(reply(&[1500], EPOCH_B, Shape::default())),
        }
    });
    h.set_query("needle");
    h.fire_debounce();
    h.step(-1);
    assert_eq!(h.requests[1].grid_epoch, EPOCH_A);
    assert_eq!(h.requests[1].before_row, Some(1744));
    // The refused resumption is discarded rather than published against a grid
    // it was not read on, and the restart asks for no epoch again.
    assert_eq!(h.requests[2].grid_epoch, String::new());
    assert_eq!(h.requests[2].before_row, None);
    assert_eq!(h.rows_with_epoch(), vec![(1500, EPOCH_B.to_string())]);
    assert!(!h.find.publication().has_failed());
}

#[test]
fn a_continuously_renumbering_pane_spends_only_one_retry() {
    let mut h = FindHarness::new();
    let mut call = 0;
    h.set_rpc(move |_, host| {
        call += 1;
        if call > 1 {
            // The pane is one grid behind the one the page is read against.
            host.anchor.grid_epoch = format!("grid-{}:0", call - 1);
        }
        RpcAnswer::Reply(reply(&[400], &format!("grid-{call}:0"), Shape::default()))
    });
    h.set_query("needle");
    h.fire_debounce();
    assert_eq!(epochs(&h), vec![String::new(), String::new()]);
    assert!(h.find.publication().has_failed());
    assert!(h.find.publication().matches().is_empty());
    assert!(h.host.jumps().is_empty());
}

#[test]
fn the_pane_epoch_gate_opens_before_adoption_and_closes_after_it() {
    let mut fresh = FindChain::new(SESSION, "seed-1", "needle", (false, false), None);
    assert!(fresh.pane_accepts_epoch(EPOCH_A));
    assert!(fresh.pane_accepts_epoch(""));
    let finished = fresh.absorb(&reply(&[400], EPOCH_B, Shape::default()), EPOCH_B, true);
    assert_eq!(adopted_epoch(finished), Some(EPOCH_B.to_string()));
    assert!(fresh.pane_accepts_epoch(EPOCH_B));
    assert!(!fresh.pane_accepts_epoch(EPOCH_A));
    // A pane that has painted no grid is the one numbering a fresh chain still
    // accepts: there is no painted row for a match to contradict.
    assert!(fresh.pane_accepts_epoch(""));

    let cursor = OlderMatchPage {
        epoch: EPOCH_A.to_string(),
        before_row: 1744,
        pages_used: 1,
    };
    let resumed = FindChain::new(SESSION, "seed-2", "needle", (false, false), Some(&cursor));
    assert!(resumed.pane_accepts_epoch(EPOCH_A));
    assert!(!resumed.pane_accepts_epoch(EPOCH_B));
    assert!(!resumed.pane_accepts_epoch(""));
}
