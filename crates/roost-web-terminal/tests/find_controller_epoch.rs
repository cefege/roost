//! Terminal-find epoch-fence tests: a hit is only revealed against the grid it
//! was found in, a retired epoch is discarded and re-searched once, and a
//! refused or mid-chain epoch move spends exactly one retry; plus the dismissal
//! contract of the find park. Test names are v2's, from
//! `apps/web/tests/renderer/terminalFindController.test.ts`.

mod find_support;

use find_support::{EPOCH_A, EPOCH_B, FindHarness, HostCall, Published, RpcAnswer, Shape, reply};
use roost_web_terminal::find::{ActiveHit, SearchStop};

fn epochs(h: &FindHarness) -> Vec<String> {
    h.requests.iter().map(|r| r.grid_epoch.clone()).collect()
}

#[test]
fn f1_a_same_epoch_hit_pulls_its_row_in_and_reveals_it() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[120], EPOCH_A, Shape::default())));
    h.set_query("boom");
    h.fire_debounce();
    assert_eq!(h.requests.len(), 1);
    assert_eq!(h.requests[0].grid_epoch, EPOCH_A);
    assert_eq!(h.rows_with_epoch(), vec![(120, EPOCH_A.to_string())]);
    assert_eq!(h.index(), 1);
    assert_eq!(h.pulled, vec![120]);
    assert_eq!(h.host.jumps(), vec![120]);
    let active = Some(ActiveHit { row: 120, col: 3 });
    assert_eq!(h.host.last(), Published { rows: vec![120], active });
}

#[test]
fn f1b_a_same_epoch_tail_match_waits_for_real_row_coverage() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[1_500], EPOCH_A, Shape::default())));
    h.set_query("tail");
    h.fire_debounce();
    assert_eq!(h.pulled, vec![1_500]);
    assert_eq!(h.host.jumps(), vec![1_500]);
}

#[test]
fn f2_a_retired_epoch_set_is_discarded_and_re_searched_before_reveal() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[1200], EPOCH_A, Shape::default())));
    h.set_query("boom");
    h.fire_debounce();
    h.host.clear_jumps();
    h.host.anchor.grid_epoch = EPOCH_B.to_string();
    h.host.anchor.sb_base = 0;
    h.host.anchor.total = 1500;
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[80], EPOCH_B, Shape::default())));
    h.step(1);
    assert_eq!(epochs(&h), vec![EPOCH_A, EPOCH_B]);
    assert!(!h.host.jumps().contains(&1200));
    assert_eq!(h.rows_with_epoch(), vec![(80, EPOCH_B.to_string())]);
    assert_eq!(h.host.jumps(), vec![80]);
}

#[test]
fn f2b_a_stale_set_stays_discarded_when_the_retry_finds_nothing() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[1200], EPOCH_A, Shape::default())));
    h.set_query("boom");
    h.fire_debounce();
    h.host.clear_jumps();
    h.host.anchor.grid_epoch = EPOCH_B.to_string();
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[], EPOCH_B, Shape::default())));
    h.step(1);
    assert_eq!(h.host.jumps(), Vec::<u32>::new());
    assert!(h.find.publication().matches().is_empty());
    assert_eq!(h.index(), 0);
    assert_eq!(h.host.last(), Published { rows: vec![], active: None });
    assert!(!h.find.publication().has_failed());
}

#[test]
fn f3_a_refused_moved_epoch_re_asks_once_against_the_displayed_grid() {
    let mut h = FindHarness::new();
    h.set_rpc(|request, host| {
        if request.grid_epoch == EPOCH_A {
            host.anchor.grid_epoch = EPOCH_B.to_string();
            return RpcAnswer::Error;
        }
        RpcAnswer::Reply(reply(&[700], EPOCH_B, Shape::default()))
    });
    h.set_query("boom");
    h.fire_debounce();
    assert_eq!(epochs(&h), vec![EPOCH_A, EPOCH_B]);
    assert!(!h.find.publication().has_failed());
    assert_eq!(h.rows(), vec![700]);
    assert_eq!(h.host.jumps(), vec![700]);
}

#[test]
fn f4_a_repeatedly_moving_epoch_spends_only_one_retry() {
    let mut h = FindHarness::new();
    let mut flip = 0;
    h.set_rpc(move |_, host| {
        flip += 1;
        host.anchor.grid_epoch = format!("grid-{flip}:0");
        RpcAnswer::Error
    });
    h.set_query("boom");
    h.fire_debounce();
    assert_eq!(h.requests.len(), 2);
    assert!(h.find.publication().has_failed());
    assert!(h.find.publication().matches().is_empty());
}

#[test]
fn f5_an_ordinary_rpc_or_regex_failure_does_not_retry() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Error);
    h.set_query("*");
    h.fire_debounce();
    assert_eq!(h.requests.len(), 1);
    assert!(h.find.publication().has_failed());
    assert!(h.find.publication().matches().is_empty());
    assert_eq!(h.host.jumps(), Vec::<u32>::new());
}

#[test]
fn later_page_epoch_change_discards_the_chain_and_retries_from_newest() {
    let mut h = FindHarness::new();
    let mut call = 0;
    h.set_rpc(move |_, host| {
        call += 1;
        RpcAnswer::Reply(match call {
            1 => reply(&[1500], EPOCH_A, Shape {
                stop: SearchStop::RowLimit,
                start: 1000,
                end: 2000,
                next: Some(1000),
            }),
            2 => {
                host.anchor.grid_epoch = EPOCH_B.to_string();
                let stop = SearchStop::EpochChanged;
                reply(&[900], EPOCH_A, Shape { stop, ..Shape::default() })
            }
            _ => reply(&[80], EPOCH_B, Shape::default()),
        })
    });
    h.set_query("moving");
    h.fire_debounce();
    assert_eq!(epochs(&h), vec![EPOCH_A, EPOCH_A, EPOCH_B]);
    let before: Vec<Option<u32>> = h.requests.iter().map(|r| r.before_row).collect();
    assert_eq!(before, vec![None, Some(1000), None]);
    assert_eq!(h.rows_with_epoch(), vec![(80, EPOCH_B.to_string())]);
    assert!(!h.find.publication().has_failed());
}

#[test]
fn a_step_republishes_the_active_highlight_before_its_reveal() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[120, 80], EPOCH_A, Shape::default())));
    h.set_query("boom");
    h.fire_debounce();
    h.host.calls.clear();
    h.step(-1);
    let active = Some(ActiveHit { row: 80, col: 3 });
    let publish = HostCall::Publish(Published { rows: vec![80, 120], active });
    assert_eq!(h.host.calls, vec![publish, HostCall::Reveal(80)]);
}

// FAILURE-INDEX "A dismissed find bar leaves the pane parked on a dead find
// anchor": dismissal ends the find reading LAST and never scrolls.
#[test]
fn closing_the_find_bar_ends_the_find_read_last_and_never_scrolls() {
    let mut h = FindHarness::new();
    h.set_rpc(|_, _| RpcAnswer::Reply(reply(&[120], EPOCH_A, Shape::default())));
    h.find.open_find();
    h.set_query("boom");
    h.fire_debounce();
    h.set_rpc(|_, _| RpcAnswer::Hold);
    h.set_query("boomer");
    h.fire_debounce();
    h.host.calls.clear();
    h.close_find();
    let cleared = HostCall::Publish(Published { rows: vec![], active: None });
    assert_eq!(h.host.calls, vec![cleared, HostCall::EndFindRead]);
    assert_eq!(h.cancellations, vec![h.held[0].search_id.clone()]);
    assert!(!h.find.is_open());
    assert_eq!(h.find.query(), "");
    let late = h.held[0].search_id.clone();
    h.resolve_held(&late, reply(&[700], EPOCH_A, Shape::default()));
    assert_eq!(h.host.jumps(), Vec::<u32>::new());
    assert!(h.find.publication().matches().is_empty());
}
