//! What a chain of sparse cell deltas must prove before it is folded at all.
//!
//! Admission is the half of the delta path that refuses: a shift is only
//! reusable when the boundary row actually matched, a link retarget defeats it,
//! and a duplicate, gapped, empty, or foreign chain folds to nothing while
//! leaving the caller's replica exactly as it was. What a valid chain writes
//! into a frame is `cell_delta_batch.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_protocol::cell::delta_batch::fold_cell_delta_batch;
use roost_protocol::cell::diff_grid::{apply_delta, delta_viewport_shift};
use roost_protocol::cell::types::{CellRow, CellSpan};
use std::sync::Arc;

use support::{full_frame, next_delta, row_text, text_row};

#[test]
fn a_shift_is_only_reused_when_the_boundary_row_actually_matched() {
    let base = full_frame(&["a", "b", "c"]);
    // A complete final viewport, but the appended line is not the held head.
    let unrelated = next_delta(
        &base,
        vec![text_row(0, "b"), text_row(1, "c"), text_row(2, "d")],
        vec![text_row(0, "somewhere else")],
    );
    assert_eq!(delta_viewport_shift(&base, &unrelated), 0);
    // A matching boundary with only part of the final viewport proves nothing.
    let partial = next_delta(&base, vec![text_row(0, "b")], vec![text_row(0, "a")]);
    assert_eq!(delta_viewport_shift(&base, &partial), 0);
    let proven = next_delta(
        &base,
        vec![text_row(0, "b"), text_row(1, "c"), text_row(2, "d")],
        vec![text_row(0, "a")],
    );
    assert_eq!(delta_viewport_shift(&base, &proven), 1);
}

#[test]
fn a_link_difference_defeats_the_shift() {
    let head = text_row(0, "a");
    let linked_head = |uri: &str| CellRow {
        index: 0,
        spans: Arc::from(vec![CellSpan {
            link_uri: Some(uri.to_owned()),
            link_key: Some("run-1".to_owned()),
            ..head.spans[0].clone()
        }]),
    };
    let mut base = full_frame(&["a", "b"]);
    base.viewport_rows[0] = linked_head("https://example.test/one");
    // Same text, same run key, different destination: genuinely different rows.
    let retargeted = next_delta(
        &base,
        vec![text_row(0, "b"), text_row(1, "c")],
        vec![linked_head("https://example.test/two")],
    );
    assert_eq!(delta_viewport_shift(&base, &retargeted), 0);
}

#[test]
fn an_empty_full_duplicate_or_gapped_chain_folds_to_nothing() {
    let base = full_frame(&["a", "b", "c"]);
    let first = next_delta(&base, vec![text_row(1, "B")], Vec::new());
    let mut duplicate = next_delta(&base, vec![text_row(0, "x"), text_row(0, "y")], Vec::new());
    duplicate.base_seq = first.seq;
    duplicate.seq = first.seq + 1;
    let mut sequence_gap = next_delta(&base, vec![text_row(2, "late")], Vec::new());
    sequence_gap.base_seq = 2;
    sequence_gap.seq = 3;
    let history_gap = next_delta(&base, Vec::new(), vec![text_row(1, "gap")]);
    let replacement = full_frame(&["x", "y", "z"]);
    let mut wrong_epoch = next_delta(&base, vec![text_row(1, "B")], Vec::new());
    wrong_epoch.grid_epoch = "other-grid:0".to_owned();

    assert!(fold_cell_delta_batch(&base, &[]).is_none());
    assert!(fold_cell_delta_batch(&base, &[replacement]).is_none());
    assert!(fold_cell_delta_batch(&base, &[first.clone(), duplicate]).is_none());
    assert!(fold_cell_delta_batch(&base, &[sequence_gap]).is_none());
    assert!(fold_cell_delta_batch(&base, &[history_gap]).is_none());
    assert!(fold_cell_delta_batch(&base, &[wrong_epoch]).is_none());

    // The baseline and every delta are exactly as they were.
    assert_eq!(row_text(&base.viewport_rows), vec!["a", "b", "c"]);
    assert_eq!(row_text(&first.viewport_rows), vec!["B"]);
    assert!(!first.full);
}

#[test]
fn a_delta_outside_its_epoch_is_refused_without_touching_the_replica() {
    let mut base = full_frame(&["a", "b"]);
    base.seq = 5;
    let mut foreign = next_delta(&base, vec![text_row(0, "B")], Vec::new());
    foreign.grid_epoch = "other-grid:0".to_owned();
    assert!(apply_delta(&mut base, &foreign).is_none());
    assert_eq!(row_text(&base.viewport_rows), vec!["a", "b"]);
    assert_eq!(base.seq, 5);

    // A sequence gap is refused the same way.
    let mut gapped = next_delta(&base, vec![text_row(0, "B")], Vec::new());
    gapped.base_seq = 3;
    assert!(apply_delta(&mut base, &gapped).is_none());
    assert_eq!(row_text(&base.viewport_rows), vec!["a", "b"]);

    // A full frame replaces the replica wholesale.
    let mut full = full_frame(&["x", "y", "z"]);
    full.seq = 99;
    assert!(apply_delta(&mut base, &full).is_some());
    assert_eq!(row_text(&base.viewport_rows), vec!["x", "y", "z"]);
    assert_eq!(base.seq, 99);
}
