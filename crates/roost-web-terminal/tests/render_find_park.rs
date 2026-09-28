//! The find park's scroll contract: it owns the anchor it scrolled to, so it
//! survives scrolling around history and the renderer's own writes and clamps;
//! a USER scroll onto the exact bottom releases it, and closing the bar
//! downgrades it without moving the view. Ported from
//! `apps/web/tests/renderer/cellRenderer.findPark.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    FakeEl, FakeRenderer, PAD_TOP, ROW_PX, delta_frame, mount, numbered_rows, row,
    seed_held_history, vp_el,
};
use roost_protocol::cell::{CellGridFrame, spans_text};
use roost_web_terminal::find::TerminalFind;
use roost_web_terminal::presentation::LiveInteractionResult;
use roost_web_terminal::{ReaderIntent, ReaderIntentReason, ReconcileBlockReason, RenderElement};

const UNCHANGED: LiveInteractionResult = LiveInteractionResult {
    reconciled: false,
    anchor_changed: false,
};

fn newer_frame() -> CellGridFrame {
    CellGridFrame {
        scrollback_total: 401,
        ..delta_frame(80, 1, vec![row(0, "latest-v")], vec![row(400, "new")], 3)
    }
}

fn pane() -> (FakeEl, FakeRenderer) {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], numbered_rows(400, 0));
    (container, renderer)
}

/// A pane parked on a find hit in mid-history, with a newer frame retained.
fn find_park() -> (FakeEl, FakeRenderer) {
    let (container, mut renderer) = pane();
    renderer.scroll_to_scrollback_row(50);
    renderer.handle_scroll();
    renderer.apply(&newer_frame());
    (container, renderer)
}

fn scroll_to_bottom(container: &FakeEl) {
    container.set_scroll_top_raw((container.scroll_height() - container.client_height()).max(0.0));
}

fn assert_still_find_parked(container: &FakeEl, renderer: &FakeRenderer) {
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Find));
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    assert_eq!(container.scroll_top_writes(), 0);
}

fn assert_resumed_to_latest(container: &FakeEl, renderer: &FakeRenderer) {
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.current_frame().unwrap().seq, 3);
    assert_eq!(vp_el(container).text_content(), "latest-v");
}

#[test]
fn a_find_park_survives_a_scroll_that_does_not_reach_the_bottom() {
    let (container, mut renderer) = find_park();
    container.set_scroll_top_raw(PAD_TOP + 120.0 * ROW_PX);
    container.reset_scroll_top_writes();
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_still_find_parked(&container, &renderer);
}

#[test]
fn a_renderer_owned_write_that_lands_at_the_bottom_keeps_the_find_park() {
    let (container, mut renderer) = pane();
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.scroll_to_scrollback_row(399);
    assert_eq!(
        container.scroll_top(),
        container.scroll_height() - container.client_height()
    );
    renderer.apply(&newer_frame());
    container.reset_scroll_top_writes();
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_still_find_parked(&container, &renderer);
}

#[test]
fn a_user_scroll_to_the_exact_bottom_resumes_a_find_park() {
    let (container, mut renderer) = find_park();
    scroll_to_bottom(&container);
    let resumed = LiveInteractionResult {
        reconciled: true,
        anchor_changed: true,
    };
    assert_eq!(renderer.handle_scroll(), resumed);
    assert_eq!(renderer.reader_reason(), None);
    assert_resumed_to_latest(&container, &renderer);
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

/// A find park at the tail whose box grew: the browser clamps it onto the
/// smaller maximum and dispatches a scroll the user never performed.
fn clamped_tail_park(grown_box_px: f64) -> (FakeEl, FakeRenderer) {
    let (container, mut renderer) = pane();
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.scroll_to_scrollback_row(399);
    renderer.handle_scroll();
    renderer.apply(&newer_frame());
    container.set_client_height(grown_box_px);
    scroll_to_bottom(&container);
    container.reset_scroll_top_writes();
    (container, renderer)
}

#[test]
fn a_box_grow_clamp_onto_the_bottom_keeps_a_find_park() {
    let (container, mut renderer) = clamped_tail_park(700.0);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_still_find_parked(&container, &renderer);
    assert_eq!(vp_el(&container).text_content(), "v");
}

#[test]
fn a_clamp_that_leaves_no_scroll_range_resumes_a_find_park() {
    let (container, mut renderer) = clamped_tail_park(6500.0);
    assert!(container.scroll_height() <= container.client_height());
    assert!(renderer.handle_scroll().reconciled);
    assert_resumed_to_latest(&container, &renderer);
}

#[test]
fn a_gesture_after_a_clamp_still_resumes_a_find_park() {
    let (container, mut renderer) = pane();
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.scroll_to_scrollback_row(300);
    renderer.handle_scroll();
    renderer.apply(&newer_frame());

    container.set_client_height(1800.0);
    scroll_to_bottom(&container);
    container.reset_scroll_top_writes();
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Find));
    assert_eq!(vp_el(&container).text_content(), "v");
    assert_eq!(container.scroll_top_writes(), 0);

    assert!(renderer.handle_scroll().reconciled);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(vp_el(&container).text_content(), "latest-v");

    renderer.scroll_to_scrollback_row(300);
    renderer.handle_scroll();
    scroll_to_bottom(&container);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.reader_reason(), None);
}

#[test]
fn closing_the_find_bar_ends_the_park_without_moving_or_painting() {
    let (container, mut renderer) = find_park();
    let mut find = TerminalFind::new("session-1", "seed");
    find.open_find();
    let parked_top = container.scroll_top();
    container.reset_scroll_top_writes();
    find.close_find(&mut renderer);
    assert_eq!(container.scroll_top(), parked_top);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(
        renderer.reader_reason(),
        Some(ReaderIntentReason::NativeScroll)
    );
    find.dispose();
}

#[test]
fn a_dismissed_find_park_follows_a_box_grow_its_anchor_would_have_refused() {
    let (container, mut renderer) = pane();
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.scroll_to_scrollback_row(399);
    renderer.handle_scroll();
    renderer.apply(&newer_frame());
    let mut find = TerminalFind::new("session-1", "seed");
    find.close_find(&mut renderer);
    container.set_client_height(700.0);
    renderer.note_box_resize();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.seq, 3);
    assert_eq!(spans_text(&frame.viewport_rows[0].spans), "latest-v");
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
    find.dispose();
}
