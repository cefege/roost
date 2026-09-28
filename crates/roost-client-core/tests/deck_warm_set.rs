//! The bounded warm-pane policy: how many panes the deck keeps mounted, which
//! one it drops first, and when an advance reports no change. Ports
//! `apps/web/tests/deckWarmSet.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_client_core::deck::{DECK_WARM_LIMIT, WarmSet};

/// s1..sN in that order; a warm set built from this reads oldest-first.
fn ids(count: usize, from: usize) -> Vec<String> {
    (0..count).map(|offset| format!("s{}", offset + from)).collect()
}

fn set(ids: &[String]) -> BTreeSet<String> {
    ids.iter().cloned().collect()
}

/// A warm set holding exactly `previous`, in that order.
fn warm_of(previous: &[String]) -> WarmSet {
    let mut warm = WarmSet::new();
    for id in previous {
        let mut open = set(warm.ids());
        open.insert(id.clone());
        warm.advance(&open, std::slice::from_ref(id), usize::MAX);
    }
    assert_eq!(warm.ids(), previous);
    warm
}

#[test]
fn a_slotted_id_is_never_evicted_even_when_it_is_the_oldest() {
    let all = ids(10, 1);
    let mut warm = warm_of(&all);
    warm.advance(&set(&all), &["s1".to_owned()], DECK_WARM_LIMIT);
    assert!(warm.contains("s1"));
    assert!(!warm.contains("s2"), "eviction took the oldest non-slotted");
    assert_eq!(warm.len(), 1 + DECK_WARM_LIMIT);
}

#[test]
fn a_split_layout_keeps_every_slotted_pane_and_caps_the_rest() {
    let all = ids(14, 1);
    let slotted = ids(3, 1);
    let mut warm = warm_of(&all);
    warm.advance(&set(&all), &slotted, DECK_WARM_LIMIT);
    for id in &slotted {
        assert!(warm.contains(id));
    }
    assert_eq!(warm.len(), slotted.len() + DECK_WARM_LIMIT);
}

#[test]
fn eviction_takes_the_least_recently_slotted_non_slotted_id_first() {
    let previous = ids(9, 1);
    let mut open = set(&previous);
    open.insert("s10".to_owned());
    let mut warm = warm_of(&previous);
    warm.advance(&open, &["s10".to_owned()], DECK_WARM_LIMIT);
    let mut expected = ids(8, 2);
    expected.push("s10".to_owned());
    assert_eq!(warm.ids(), expected, "s1 gone, s10 newest");
}

#[test]
fn being_slotted_refreshes_recency_so_an_older_pane_outlives_a_newer_one() {
    let open = set(&ids(11, 1));
    let mut warm = warm_of(&ids(9, 1));
    warm.advance(&open, &["s2".to_owned()], DECK_WARM_LIMIT);
    let mut expected = vec!["s1".to_owned()];
    expected.extend(ids(7, 3));
    expected.push("s2".to_owned());
    assert_eq!(warm.ids(), expected, "s2 shown, so it is newest");
    warm.advance(&open, &["s10".to_owned()], DECK_WARM_LIMIT);
    assert!(!warm.contains("s1"), "over the cap drops the oldest");
    warm.advance(&open, &["s11".to_owned()], DECK_WARM_LIMIT);
    assert!(!warm.contains("s3"));
    assert!(warm.contains("s2"), "inserted before s3, shown after it");
}

#[test]
fn a_closed_id_disappears_without_counting_against_the_limit() {
    let previous = ids(9, 1);
    let mut open_ids = ids(6, 4);
    open_ids.push("s10".to_owned());
    let mut warm = warm_of(&previous);
    warm.advance(&set(&open_ids), &["s10".to_owned()], DECK_WARM_LIMIT);
    assert_eq!(warm.ids(), open_ids);
    assert_eq!(warm.len(), 7, "three closures evicted nobody else");
}

#[test]
fn a_slot_naming_a_session_that_is_not_open_is_not_warmed() {
    let mut warm = WarmSet::new();
    warm.advance(
        &set(&["s1".to_owned()]),
        &["ghost".to_owned(), "s1".to_owned()],
        DECK_WARM_LIMIT,
    );
    assert_eq!(warm.ids(), ["s1".to_owned()]);
}

#[test]
fn membership_never_exceeds_one_slot_plus_the_limit() {
    let all = ids(40, 1);
    let open = set(&all);
    let mut warm = WarmSet::new();
    for id in &all {
        warm.advance(&open, std::slice::from_ref(id), DECK_WARM_LIMIT);
        assert!(warm.len() <= 1 + DECK_WARM_LIMIT);
        assert!(warm.contains(id));
    }
}

#[test]
fn an_explicit_limit_caps_the_non_slotted_panes() {
    let all = ids(5, 1);
    let mut warm = warm_of(&all);
    warm.advance(&set(&all), &["s5".to_owned()], 0);
    assert_eq!(warm.ids(), ["s5".to_owned()], "limit 0 keeps only what is on screen");
}

#[test]
fn the_same_inputs_report_no_change() {
    let open = set(&ids(4, 1));
    let mut warm = warm_of(&ids(3, 1));
    assert!(warm.advance(&open, &["s4".to_owned()], DECK_WARM_LIMIT));
    let first = warm.clone();
    assert!(!warm.advance(&open, &["s4".to_owned()], DECK_WARM_LIMIT));
    assert!(!warm.advance(&open, &["s4".to_owned()], DECK_WARM_LIMIT));
    assert_eq!(warm, first);
}

#[test]
fn a_pure_reorder_is_a_change() {
    let both = vec!["a".to_owned(), "b".to_owned()];
    let mut warm = warm_of(&both);
    assert!(
        warm.advance(&set(&both), &["a".to_owned()], DECK_WARM_LIMIT),
        "recency moved: a is now newest"
    );
    assert_eq!(warm.ids(), ["b".to_owned(), "a".to_owned()]);
}
