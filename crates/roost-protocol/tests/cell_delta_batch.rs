//! Folding sparse cell deltas into one frame: sequence admission, viewport
//! shift reuse, final row coordinates, and history order.
//!
//! The fixtures are hand-built rows rather than an encoder's output, so the
//! fold is exercised without a terminal core. Two properties matter beyond the
//! reconstructed grid: a shift is only reused when the delta proves it, and
//! neither the baseline nor any supplied delta is mutated — a chain that turns
//! out invalid must leave the caller's replica exactly as it was. What a chain
//! must prove before it folds at all is `cell_delta_admission.rs`.
mod support;

use std::sync::Arc;

use roost_protocol::cell::delta_batch::{CellDeltaBatch, fold_cell_delta_batch};
use roost_protocol::cell::diff_grid::{clone_cell_grid_frame, normalize_cell_grid_frame};
use roost_protocol::cell::types::CellGridFrame;

use support::{full_frame, indices, next_delta, row_text, text_row};

fn folded(base: &CellGridFrame, deltas: &[CellGridFrame]) -> CellDeltaBatch {
    match fold_cell_delta_batch(base, deltas) {
        Some(batch) => batch,
        None => panic!("the chain must fold"),
    }
}

#[test]
fn one_sparse_delta_lands_in_one_owned_successor() {
    let base = full_frame(&["zero", "one", "two"]);
    let delta = next_delta(&base, vec![text_row(1, "ONE")], Vec::new());
    let batch = folded(&base, std::slice::from_ref(&delta));

    assert_eq!(
        (batch.frame.full, batch.frame.base_seq, batch.frame.seq),
        (true, 0, 2)
    );
    assert_eq!(
        row_text(&batch.frame.viewport_rows),
        vec!["zero", "ONE", "two"]
    );
    assert_eq!(indices(&batch.dirty_rows), vec![1]);
    assert_eq!(indices(&batch.scrollback_append), Vec::<u32>::new());
    assert_eq!(batch.viewport_shift, 0);
    // The successor shares the delta's cells rather than copying them, and the
    // caller's baseline is untouched.
    assert!(Arc::ptr_eq(
        &batch.frame.viewport_rows[1].spans,
        &delta.viewport_rows[0].spans
    ));
    assert_eq!(row_text(&base.viewport_rows), vec!["zero", "one", "two"]);
}

#[test]
fn a_clone_gives_independent_row_coordinates_over_shared_cells() {
    let base = full_frame(&["a", "b"]);
    let clone = clone_cell_grid_frame(&base);
    assert_eq!(clone, base);
    // The row object is fresh; the cells inside it are the very same array, so a
    // renderer that already painted them is not charged for the clone.
    assert!(Arc::ptr_eq(
        &clone.viewport_rows[0].spans,
        &base.viewport_rows[0].spans
    ));
    assert_eq!(row_text(&clone.viewport_rows), vec!["a", "b"]);
}

#[test]
fn normalizing_a_checkpoint_severs_the_inbound_history_alias() {
    let mut canonical = full_frame(&["a", "b"]);
    canonical.scrollback_rows = vec![text_row(0, "old")];
    canonical.scrollback_total = 1;
    canonical.sb_base = 0;
    canonical.seq = 42;
    let incoming = vec![text_row(1, "new")];
    canonical.scrollback_append = incoming.clone();

    normalize_cell_grid_frame(&mut canonical);

    assert_eq!(
        (
            canonical.full,
            canonical.base_seq,
            canonical.scrollback_total,
            canonical.sb_base,
            canonical.seq
        ),
        (true, 0, 1, 1, 42)
    );
    assert!(canonical.scrollback_rows.is_empty());
    assert!(canonical.scrollback_append.is_empty());
    // The delta's own array still holds what it arrived with.
    assert_eq!(indices(&incoming), vec![1]);
}

#[test]
fn sparse_deltas_combine_in_final_order_and_keep_the_latest_metadata() {
    let base = full_frame(&["a", "b", "c"]);
    let first = next_delta(&base, vec![text_row(0, "A")], Vec::new());
    let mut second = next_delta(&base, vec![text_row(2, "C")], Vec::new());
    second.base_seq = first.seq;
    second.seq = first.seq + 1;
    second.cursor_col = 7;
    let batch = folded(&base, &[first, second]);

    assert_eq!((batch.frame.seq, batch.frame.cursor_col), (3, 7));
    assert_eq!(row_text(&batch.frame.viewport_rows), vec!["A", "b", "C"]);
    assert_eq!(indices(&batch.dirty_rows), vec![0, 2]);
    assert!(batch.frame.scrollback_rows.is_empty());
    assert!(batch.frame.scrollback_append.is_empty());
}

#[test]
fn a_proven_shift_translates_final_rows_and_keeps_history_order() {
    let base = full_frame(&["a", "b", "c", "d"]);
    let first = next_delta(
        &base,
        vec![
            text_row(0, "b"),
            text_row(1, "C!"),
            text_row(2, "d"),
            text_row(3, "e"),
        ],
        vec![text_row(0, "a")],
    );
    let mut second = next_delta(
        &base,
        vec![
            text_row(0, "C!"),
            text_row(1, "d"),
            text_row(2, "E!"),
            text_row(3, "f"),
        ],
        vec![text_row(1, "b")],
    );
    second.base_seq = first.seq;
    second.seq = first.seq + 1;
    second.scrollback_total = first.scrollback_total + 1;
    let batch = folded(&base, &[first, second]);

    assert_eq!(batch.viewport_shift, 2);
    // Every row is marked: each shift moved the marks and each delta arrived
    // with the complete final viewport.
    assert_eq!(indices(&batch.dirty_rows), vec![0, 1, 2, 3]);
    assert_eq!(row_text(&batch.dirty_rows), vec!["C!", "d", "E!", "f"]);
    assert_eq!(
        row_text(&batch.frame.viewport_rows),
        vec!["C!", "d", "E!", "f"]
    );
    assert_eq!(batch.frame.scrollback_total, 2);
    assert_eq!(row_text(&batch.frame.scrollback_rows), vec!["a", "b"]);
    assert_eq!(indices(&batch.scrollback_append), vec![0, 1]);
    assert_eq!(row_text(&batch.scrollback_append), vec!["a", "b"]);
}

#[test]
fn a_shift_beyond_the_held_history_marks_the_whole_viewport() {
    // Two lines arrive for a two-row grid: the shift is capped at the grid
    // height, every original row is discarded, and the final viewport is the
    // patch. A shift below the height leaves the untouched rows out of it.
    let base = full_frame(&["a", "b"]);
    let first = next_delta(
        &base,
        vec![text_row(0, "b"), text_row(1, "c")],
        vec![text_row(0, "a")],
    );
    let mut second = next_delta(
        &base,
        vec![text_row(0, "c"), text_row(1, "d")],
        vec![text_row(1, "b")],
    );
    second.base_seq = first.seq;
    second.seq = first.seq + 1;
    second.scrollback_total = first.scrollback_total + 1;
    let batch = folded(&base, &[first, second]);

    assert_eq!(batch.viewport_shift, 2);
    assert_eq!(row_text(&batch.dirty_rows), vec!["c", "d"]);

    // One line into a four-row grid shifts by one and keeps the rest of the
    // held viewport out of the patch.
    let tall = full_frame(&["a", "b", "c", "d"]);
    let scrolled = next_delta(
        &tall,
        vec![
            text_row(0, "b"),
            text_row(1, "c"),
            text_row(2, "d"),
            text_row(3, "e"),
        ],
        vec![text_row(0, "a")],
    );
    let batch = folded(&tall, std::slice::from_ref(&scrolled));
    assert_eq!(batch.viewport_shift, 1);
    assert_eq!(
        row_text(&batch.frame.viewport_rows),
        vec!["b", "c", "d", "e"]
    );
}

#[test]
fn a_sparse_partial_region_batch_preserves_the_untouched_footer() {
    // A TUI repaints a block in place while a panel below it stays put. The
    // scroll boundary "matches", but the delta does not carry the complete final
    // viewport, so the shift is not reusable and the footer keeps its row.
    let base = full_frame(&[
        "HEAD-0",
        "GREP-HEAD",
        "README.md#8C59",
        "BODY-A",
        "FIXED-PANEL",
        "STATUS-000",
    ]);
    let first = next_delta(
        &base,
        vec![
            text_row(0, "GREP-HEAD"),
            text_row(1, "README.md#8C59"),
            text_row(2, "BODY-A"),
            text_row(3, "NEXT"),
            text_row(5, "STATUS-001"),
        ],
        vec![text_row(0, "HEAD-0")],
    );
    let mut second = next_delta(&base, vec![text_row(5, "STATUS-002")], Vec::new());
    second.base_seq = first.seq;
    second.seq = first.seq + 1;
    second.scrollback_total = first.scrollback_total;
    let batch = folded(&base, &[first, second]);

    assert_eq!(batch.viewport_shift, 0);
    assert_eq!(
        row_text(&batch.frame.viewport_rows),
        vec![
            "GREP-HEAD",
            "README.md#8C59",
            "BODY-A",
            "NEXT",
            "FIXED-PANEL",
            "STATUS-002"
        ]
    );
    assert_eq!(indices(&batch.dirty_rows), vec![0, 1, 2, 3, 5]);
    assert_eq!(row_text(&batch.frame.scrollback_rows), vec!["HEAD-0"]);
    assert_eq!(indices(&batch.scrollback_append), vec![0]);
}
