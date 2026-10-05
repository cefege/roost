//! The append-only scrollback tripwire, ported from
//! `apps/web/tests/renderer/cellRenderer.append.dom.test.ts`: scrollback rows
//! are immutable and append-only, the painted width is the worker's `cols`,
//! and existing history nodes survive every delta — locked by NODE IDENTITY,
//! because a delta path that silently became a full re-render would otherwise
//! stay green.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{PAD_TOP, ROW_PX, delta_frame, mount, row, sb_el, sb_rows, seed_held_history};
use roost_protocol::cell::{CellGridFrame, CellRow, spans_text};
use roost_web_terminal::ReaderIntent;

fn history_rows(count: u32) -> Vec<CellRow> {
    (0..count)
        .map(|index| row(index, &format!("h{index}")))
        .collect()
}

fn held_history_text(frame: &CellGridFrame) -> Vec<String> {
    frame
        .scrollback_rows
        .iter()
        .map(|held| spans_text(&held.spans))
        .collect()
}

/// Deltas `seq..` each pushing the next history row onto `total` held rows.
fn appending_deltas(total: u32, seq: u64, count: u32) -> Vec<CellGridFrame> {
    (0..count)
        .map(|offset| {
            let index = total + offset;
            CellGridFrame {
                scrollback_total: u64::from(index + 1),
                ..delta_frame(
                    80,
                    1,
                    vec![row(0, &format!("v{index}"))],
                    vec![row(index, &format!("h{index}"))],
                    seq + u64::from(offset),
                )
            }
        })
        .collect()
}

#[test]
fn live_deltas_keep_the_held_history_and_extend_it_in_order() {
    let (_container, mut renderer) = mount();
    assert!(seed_held_history(
        &mut renderer,
        80,
        vec![row(0, "v")],
        history_rows(10)
    ));

    for delta in appending_deltas(10, 3, 3) {
        assert!(renderer.apply(&delta));
    }

    let expected: Vec<String> = (0..13).map(|index| format!("h{index}")).collect();
    assert_eq!(
        held_history_text(renderer.current_frame().unwrap()),
        expected
    );
}

#[test]
fn deltas_folded_during_a_reading_park_keep_the_held_history() {
    let (container, mut renderer) = mount();
    assert!(seed_held_history(
        &mut renderer,
        80,
        vec![row(0, "v")],
        history_rows(400)
    ));
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    renderer.handle_scroll();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);

    for delta in appending_deltas(400, 3, 3) {
        assert!(renderer.apply(&delta));
    }
    // The painted frame is untouched while the reader holds it.
    assert_eq!(renderer.current_frame().unwrap().scrollback_rows.len(), 400);

    renderer.prepare_live_interaction();
    let expected: Vec<String> = (0..403).map(|index| format!("h{index}")).collect();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(
        held_history_text(renderer.current_frame().unwrap()),
        expected
    );
}

#[test]
fn a_delta_appends_scrollback_and_existing_rows_keep_their_identity() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    seed_held_history(
        &mut renderer,
        80,
        vec![row(0, "v0"), row(1, "v1")],
        vec![row(0, "h0"), row(1, "h1")],
    );
    let before = sb_rows(&scrollback);
    assert_eq!(before.len(), 2);

    let mut delta = delta_frame(80, 2, vec![row(1, "v1b")], vec![row(2, "h2")], 3);
    delta.scrollback_total = 3;
    renderer.apply(&delta);

    let after = sb_rows(&scrollback);
    assert_eq!(after.len(), 3);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[1]);
    assert_ne!(after[2], before[1]);
}

#[test]
fn canonical_frame_seq_tracks_each_exactly_sequenced_accepted_frame() {
    let (_container, mut renderer) = mount();
    assert_eq!(renderer.canonical_frame_seq(), 0);

    seed_held_history(&mut renderer, 80, vec![row(0, "v0")], Vec::new());
    assert_eq!(renderer.canonical_frame_seq(), 1);

    let mut delta = delta_frame(80, 1, vec![row(0, "v0b")], vec![row(0, "h1")], 2);
    delta.scrollback_total = 1;
    renderer.apply(&delta);
    assert_eq!(renderer.canonical_frame_seq(), 2);

    let mut delta = delta_frame(80, 1, vec![row(0, "v0c")], Vec::new(), 3);
    delta.scrollback_total = 1;
    renderer.apply(&delta);
    assert_eq!(renderer.canonical_frame_seq(), 3);
}

#[test]
fn a_delta_from_a_different_grid_epoch_is_rejected() {
    let (_container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "held")], Vec::new());
    let mut stale = delta_frame(80, 1, vec![row(0, "wrong")], Vec::new(), 2);
    stale.grid_epoch = "test-grid:1".to_string();

    assert!(!renderer.apply(&stale));
    assert_eq!(renderer.current_frame().map(|frame| frame.seq), Some(1));
    assert_eq!(renderer.grid_text(), "held");
}

#[test]
fn a_viewport_only_delta_does_not_touch_scrollback_dom() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    seed_held_history(
        &mut renderer,
        80,
        vec![row(0, "v0")],
        vec![row(0, "h0"), row(1, "h1")],
    );
    let before = sb_rows(&scrollback);

    let mut delta = delta_frame(80, 1, vec![row(0, "v0-changed")], Vec::new(), 2);
    delta.scrollback_total = 2;
    renderer.apply(&delta);

    let after = sb_rows(&scrollback);
    assert_eq!(after.len(), 2);
    assert_eq!(after[0], before[0]);
    assert_eq!(after[1], before[1]);
}
