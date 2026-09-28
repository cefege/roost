//! Box geometry: a resize reconciles synchronously (a ResizeObserver can run
//! after layout and after another frame), explicit off-bottom reading stays
//! untouched, and a grow that leaves no scroll range is a parked pane's only
//! resume. Ported from the box-resize cases of
//! `apps/web/tests/renderer/cellRenderer.geometry.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    FakeEl, FakeRenderer, PAD_TOP, ROW_PX, full_frame, mount, row, sb_el, seed_held_history,
    seed_held_history_to, vp_el,
};
use roost_protocol::cell::{CellGridFrame, CellRow, spans_text};
use roost_web_terminal::presentation::LiveInteractionResult;
use roost_web_terminal::{
    RENDERER_HOLD_SELECTION, ReaderIntent, ReaderIntentReason, ReconcileBlockReason, RenderElement,
};

const UNCHANGED: LiveInteractionResult = LiveInteractionResult {
    reconciled: false,
    anchor_changed: false,
};
const RESUMED: LiveInteractionResult = LiveInteractionResult {
    reconciled: true,
    anchor_changed: true,
};

fn rows(count: u32, from: u32) -> Vec<CellRow> {
    (from..from + count)
        .map(|index| row(index, &format!("b{index}")))
        .collect()
}

fn bottom_of(container: &FakeEl) -> f64 {
    container.scroll_height() - container.client_height()
}

fn full(viewport_text: &str, total: u64, seq: u64) -> CellGridFrame {
    CellGridFrame {
        seq,
        ..full_frame(80, vec![row(0, viewport_text)], total)
    }
}

fn pane(viewport_text: &str) -> (FakeEl, FakeRenderer) {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, viewport_text)], rows(400, 0));
    (container, renderer)
}

#[test]
fn a_live_old_bottom_anchor_follows_a_box_shrink_with_exactly_one_pin() {
    let (container, mut renderer) = pane("v");
    container.set_scroll_top_raw(bottom_of(&container).max(0.0));
    container.reset_scroll_top_writes();
    container.set_client_height(400.0);
    renderer.note_box_resize();
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
    assert!(renderer.at_bottom());
    renderer.note_box_resize();
    assert_eq!(container.scroll_top_writes(), 1);
}

#[test]
fn an_old_bottom_resize_reconciles_a_frame_that_arrived_after_layout_without_scroll() {
    let (container, mut renderer) = pane("old");
    container.set_scroll_top_raw(bottom_of(&container).max(0.0));
    renderer.handle_scroll();
    container.set_client_height(400.0);
    renderer.handle_scroll();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert!(renderer.apply(&CellGridFrame {
        sb_base: 410,
        ..full("after-layout", 410, 3)
    }));
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    container.reset_scroll_top_writes();

    assert_eq!(renderer.note_box_resize(), RESUMED);
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.seq, 3);
    assert_eq!(spans_text(&frame.viewport_rows[0].spans), "after-layout");
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
}

#[test]
fn a_pending_owned_bottom_placement_repins_a_compatible_full_after_late_geometry() {
    let (container, mut renderer) = pane("old");
    container.set_scroll_top_raw(bottom_of(&container) - ROW_PX);
    renderer.prepare_live_interaction();
    container.set_client_height(container.client_height() - ROW_PX);
    assert!(!renderer.at_bottom());
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    container.reset_scroll_top_writes();
    assert!(renderer.apply(&full("after-layout", 401, 3)));
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
}

#[test]
fn a_user_scroll_clears_a_late_owned_placement_before_a_compatible_full() {
    let (container, mut renderer) = pane("old");
    container.set_scroll_top_raw(bottom_of(&container) - ROW_PX);
    renderer.prepare_live_interaction();
    container.set_client_height(container.client_height() - ROW_PX);
    renderer.handle_scroll();
    let reader_top = container.scroll_top() - 3.0 * ROW_PX;
    container.set_scroll_top_raw(reader_top);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    container.reset_scroll_top_writes();
    assert!(renderer.apply(&full("new", 401, 3)));
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    assert_eq!(container.scroll_top(), reader_top);
    assert_eq!(container.scroll_top_writes(), 0);
}

#[test]
fn an_off_bottom_reader_is_untouched_by_a_box_shrink() {
    let (container, mut renderer) = pane("v");
    container.set_scroll_top_raw(bottom_of(&container) - 2.0);
    let before = container.scroll_top();
    container.reset_scroll_top_writes();
    container.set_client_height(400.0);
    renderer.note_box_resize();
    assert_eq!(container.scroll_top(), before);
    assert_eq!(container.scroll_top_writes(), 0);
}

#[test]
fn an_at_bottom_reader_follows_a_box_grow_onto_the_new_bottom() {
    let (container, mut renderer) = pane("v");
    container.set_scroll_top_raw(bottom_of(&container).max(0.0));
    container.reset_scroll_top_writes();
    container.set_client_height(700.0);
    renderer.note_box_resize();
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
    assert!(renderer.at_bottom());
}

/// Park with `reason`, retain a newer frame off-DOM, and hand back the box.
fn parked_pane(
    reason: ReaderIntentReason,
    history_rows: u32,
    box_px: f64,
    park_row: Option<f64>,
) -> (FakeEl, FakeRenderer) {
    let (container, mut renderer) = mount();
    container.set_client_height(box_px);
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(history_rows, 0));
    let top = park_row.map_or_else(
        || bottom_of(&container),
        |park_row| PAD_TOP + park_row * ROW_PX,
    );
    container.set_scroll_top_raw(top);
    renderer.enter_reading(reason);
    renderer.apply(&full("latest", u64::from(history_rows) + 10, 3));
    container.reset_scroll_top_writes();
    (container, renderer)
}

#[test]
fn a_wheel_parked_reader_resumes_when_a_box_grow_leaves_no_scroll_range() {
    let (container, mut renderer) = parked_pane(ReaderIntentReason::Wheel, 20, 100.0, Some(4.0));
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    assert_eq!(vp_el(&container).text_content(), "v");
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::ReaderPendingFrame
    );
    container.set_client_height(400.0);
    assert!(container.scroll_height() <= container.client_height());
    renderer.note_box_resize();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.current_frame().unwrap().seq, 3);
    assert_eq!(vp_el(&container).text_content(), "latest");
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn a_wheel_reader_off_the_old_bottom_keeps_its_park_across_a_box_grow() {
    let (container, mut renderer) = parked_pane(ReaderIntentReason::Wheel, 400, 500.0, Some(100.0));
    let parked = container.scroll_top();
    container.set_client_height(700.0);
    renderer.note_box_resize();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Wheel));
    assert_eq!(vp_el(&container).text_content(), "v");
    assert_eq!(container.scroll_top(), parked);
    assert_eq!(container.scroll_top_writes(), 0);
}

#[test]
fn a_find_park_resumes_only_when_the_grow_leaves_no_scroll_range() {
    let (unreachable, mut resumed) = parked_pane(ReaderIntentReason::Find, 20, 100.0, Some(4.0));
    unreachable.set_client_height(400.0);
    resumed.note_box_resize();
    assert_eq!(resumed.reader_intent(), ReaderIntent::Live);
    assert_eq!(vp_el(&unreachable).text_content(), "latest");

    let (ranged, mut kept) = parked_pane(ReaderIntentReason::Find, 400, 500.0, None);
    ranged.set_client_height(700.0);
    kept.note_box_resize();
    assert_eq!(kept.reader_intent(), ReaderIntent::Reading);
    assert_eq!(kept.reader_reason(), Some(ReaderIntentReason::Find));
    assert_eq!(vp_el(&ranged).text_content(), "v");
    assert_eq!(ranged.scroll_top_writes(), 0);
}

#[test]
fn a_held_pane_keeps_its_reader_identity_when_a_scroll_returns_to_the_bottom() {
    let (container, mut renderer) = pane("v");
    let bottom = bottom_of(&container);
    container.set_scroll_top_raw(bottom);
    renderer.handle_scroll();
    renderer.set_selection_hold(true);
    container.set_scroll_top_raw(bottom - 5.0 * ROW_PX);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    container.set_scroll_top_raw(bottom);
    container.reset_scroll_top_writes();
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(
        renderer.reader_reason(),
        Some(ReaderIntentReason::Selection)
    );
    assert_eq!(renderer.hold_mask(), RENDERER_HOLD_SELECTION);
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::SelectionHold
    );
    assert_eq!(container.scroll_top_writes(), 0);

    assert!(renderer.apply(&full("held-latest", 410, 3)));
    assert_eq!(vp_el(&container).text_content(), "v");
    assert_eq!(renderer.set_selection_hold(false), RESUMED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(vp_el(&container).text_content(), "held-latest");
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn an_epoch_changing_full_frame_waits_off_dom_during_explicit_reading() {
    let (container, mut renderer) = mount();
    seed_held_history_to(&mut renderer, 80, vec![row(0, "v")], rows(250, 500), 750);
    container.set_scroll_top_raw(PAD_TOP + 600.0 * ROW_PX);
    renderer.handle_scroll();
    let before = container.scroll_top();
    container.reset_scroll_top_writes();
    renderer.apply(&CellGridFrame {
        grid_epoch: "test-grid:1".to_string(),
        ..full("v", 5000, 3)
    });
    assert_eq!(container.scroll_top(), before);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(renderer.current_frame().unwrap().grid_epoch, "test-grid:0");
    assert_eq!(renderer.canonical_frame_seq(), 3);
    assert_eq!(renderer.prepare_live_interaction(), RESUMED);
    assert_eq!(renderer.current_frame().unwrap().grid_epoch, "test-grid:1");
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
}

#[test]
fn render_full_reserves_the_incoming_spacer_before_wiping_painted_history() {
    let (container, mut renderer) = mount();
    seed_held_history_to(&mut renderer, 80, vec![row(0, "v")], rows(250, 500), 750);
    container.set_scroll_top_raw(bottom_of(&container).max(0.0));
    let pre_scroll_top = container.scroll_top();
    let wider = CellGridFrame {
        grid_epoch: "test-grid:1".to_string(),
        seq: 3,
        ..full_frame(100, vec![row(0, "v")], 6000)
    };
    renderer.apply(&wider);
    let (spacer_at_wipe, height_at_wipe) = sb_el(&container)
        .last_wipe()
        .expect("the history sheet was wiped");
    assert_eq!(spacer_at_wipe, 6000.0 * ROW_PX);
    assert!(height_at_wipe >= pre_scroll_top);
}
