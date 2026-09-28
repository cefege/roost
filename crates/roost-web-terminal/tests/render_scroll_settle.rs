//! The native-scroll settle: layout can clamp a reader onto the literal bottom
//! with no second scroll event, so the renderer arms a bottom-park settle the
//! pane runs next frame, and a band rest ASKS the pane for one settle window.
//! Ported from `apps/web/tests/renderer/cellRenderer.nativeScrollSettle.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use std::cell::Cell;
use std::rc::Rc;

use render_support::{
    FakeEl, FakeRenderer, ROW_PX, delta_frame, history_node, row, sb_el, sb_rows, seed_held_history,
};
use roost_protocol::cell::{CellGridFrame, CellRow};
use roost_web_terminal::presentation::LiveInteractionResult;
use roost_web_terminal::{
    CellGridRenderer, ReaderIntent, ReaderIntentReason, ReconcileBlockReason, RenderElement,
};

const UNCHANGED: LiveInteractionResult = LiveInteractionResult {
    reconciled: false,
    anchor_changed: false,
};

fn history(count: u32) -> Vec<CellRow> {
    (0..count)
        .map(|index| row(index, &format!("s{index}")))
        .collect()
}

fn appended_frame(seq: u64) -> CellGridFrame {
    let appended = u32::try_from(seq).unwrap() + 397;
    CellGridFrame {
        scrollback_total: 398 + seq,
        ..delta_frame(
            80,
            1,
            vec![row(0, "v")],
            vec![row(appended, "after-settle")],
            seq,
        )
    }
}

/// The pane's side of the contract: run the armed settle on the next frame.
fn drain_animation_frames(renderer: &mut FakeRenderer) {
    for _ in 0..8 {
        let Some(epoch) = renderer.pending_bottom_park_settle() else {
            return;
        };
        renderer.resume_bottom_park(epoch);
    }
}

/// The pane's quiet window: a frame may OPEN one, and further frames must not
/// re-arm it.
struct SettleWindow {
    pending: Rc<Cell<bool>>,
    opened: Rc<Cell<u32>>,
}

fn pane(settle: Option<&SettleWindow>) -> (FakeEl, FakeRenderer) {
    let container = FakeEl::container();
    let request = settle.map(|window| {
        let (pending, opened) = (window.pending.clone(), window.opened.clone());
        Box::new(move || {
            if !pending.replace(true) {
                opened.set(opened.get() + 1);
            }
        }) as Box<dyn Fn()>
    });
    let mut renderer = CellGridRenderer::with_callbacks(&container, None, None, request).unwrap();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], history(400));
    (container, renderer)
}

fn settle_window() -> SettleWindow {
    SettleWindow {
        pending: Rc::new(Cell::new(false)),
        opened: Rc::new(Cell::new(0)),
    }
}

fn bottom_of(container: &FakeEl) -> f64 {
    container.scroll_height() - container.client_height()
}

fn assert_live_and_reconciled(renderer: &FakeRenderer, seq: u64) {
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(
        renderer.canonical_epoch_seq(),
        renderer.reconciled_epoch_seq()
    );
    assert_eq!(renderer.current_frame().unwrap().seq, seq);
}

#[test]
fn native_scroll_settle_resumes_a_frame_after_silent_bottom_clamping() {
    let (container, mut renderer) = pane(None);
    container.set_scroll_top_raw(bottom_of(&container) - 3.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.apply(&appended_frame(3));
    drain_animation_frames(&mut renderer);
    container.set_scroll_top_raw(bottom_of(&container));
    renderer.apply(&appended_frame(4));
    drain_animation_frames(&mut renderer);
    assert_live_and_reconciled(&renderer, 4);
}

#[test]
fn a_wheel_park_clamped_to_the_bottom_settles_without_a_second_scroll_event() {
    let (container, mut renderer) = pane(None);
    container.set_scroll_top_raw(bottom_of(&container));
    renderer.enter_reading(ReaderIntentReason::Wheel);
    renderer.apply(&appended_frame(3));
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    drain_animation_frames(&mut renderer);
    assert_live_and_reconciled(&renderer, 3);
}

#[test]
fn a_wheel_park_clamped_onto_the_bottom_by_a_box_grow_resumes() {
    let (container, mut renderer) = pane(None);
    container.set_scroll_top_raw(bottom_of(&container) - 4.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.enter_reading(ReaderIntentReason::Wheel);
    renderer.apply(&appended_frame(3));
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    container.set_client_height(700.0);
    container.set_scroll_top_raw(bottom_of(&container));
    assert!(renderer.handle_scroll().reconciled);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.current_frame().unwrap().seq, 3);
}

#[test]
fn native_scroll_settle_keeps_a_true_off_bottom_reader_held() {
    let (container, mut renderer) = pane(None);
    container.set_scroll_top_raw(bottom_of(&container) - 3.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.apply(&appended_frame(3));
    drain_animation_frames(&mut renderer);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(
        renderer.reader_reason(),
        Some(ReaderIntentReason::NativeScroll)
    );
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::ReaderPendingFrame
    );
}

#[test]
fn a_wheel_park_resting_inside_the_follow_band_resumes_on_the_settle() {
    let (container, mut renderer) = pane(None);
    renderer.enter_reading(ReaderIntentReason::Wheel);
    container.set_scroll_top_raw(bottom_of(&container) - ROW_PX);
    renderer.handle_scroll();
    renderer.apply(&appended_frame(3));
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::ReaderPendingFrame
    );
    assert!(renderer.settle_follow_band().reconciled);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn a_park_beyond_the_follow_band_survives_the_settle() {
    let (container, mut renderer) = pane(None);
    container.set_scroll_top_raw(bottom_of(&container) - 3.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.apply(&appended_frame(3));
    assert_eq!(renderer.settle_follow_band(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(
        renderer.reader_reason(),
        Some(ReaderIntentReason::NativeScroll)
    );
}

#[test]
fn a_find_park_inside_the_follow_band_keeps_its_anchor_through_the_settle() {
    let (container, mut renderer) = pane(None);
    container.set_scroll_top_raw(bottom_of(&container) - ROW_PX);
    renderer.enter_reading(ReaderIntentReason::Find);
    assert_eq!(renderer.settle_follow_band(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Find));
}

#[test]
fn a_band_rest_parked_with_no_scroll_event_recruits_the_settle_on_a_frame() {
    let settle = settle_window();
    let (container, mut renderer) = pane(Some(&settle));
    container.set_scroll_top_raw(bottom_of(&container) - ROW_PX);
    renderer.enter_reading(ReaderIntentReason::Wheel);
    container.reset_scroll_top_writes();
    renderer.apply(&appended_frame(3));
    drain_animation_frames(&mut renderer);
    assert_eq!(settle.opened.get(), 1);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::ReaderPendingFrame
    );
    assert!(renderer.settle_follow_band().reconciled);
    assert_live_and_reconciled(&renderer, 3);
    assert_eq!(history_node(&container, 400).text_content(), "after-settle");
}

#[test]
fn a_park_beyond_the_follow_band_recruits_no_settle_window() {
    let settle = settle_window();
    let (container, mut renderer) = pane(Some(&settle));
    container.set_scroll_top_raw(bottom_of(&container) - 3.0 * ROW_PX);
    renderer.enter_reading(ReaderIntentReason::Wheel);
    renderer.apply(&appended_frame(3));
    drain_animation_frames(&mut renderer);
    assert_eq!(settle.opened.get(), 0);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Wheel));
    assert_eq!(renderer.current_frame().unwrap().seq, 2);
    assert_eq!(
        sb_rows(&sb_el(&container)).last().unwrap().text_content(),
        "s399"
    );
}

#[test]
fn a_park_on_the_exact_clamp_resumes_on_the_frame_with_no_settle_window() {
    let settle = settle_window();
    let (container, mut renderer) = pane(Some(&settle));
    container.set_scroll_top_raw(bottom_of(&container));
    renderer.enter_reading(ReaderIntentReason::Wheel);
    renderer.apply(&appended_frame(3));
    drain_animation_frames(&mut renderer);
    assert_eq!(settle.opened.get(), 0);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.current_frame().unwrap().seq, 3);
}

#[test]
fn a_stream_of_frames_over_a_band_rest_keeps_exactly_one_settle_window() {
    let settle = settle_window();
    let (container, mut renderer) = pane(Some(&settle));
    container.set_scroll_top_raw(bottom_of(&container) - ROW_PX);
    renderer.enter_reading(ReaderIntentReason::Wheel);
    for seq in 3..=8 {
        assert!(renderer.apply(&appended_frame(seq)));
        drain_animation_frames(&mut renderer);
    }
    assert_eq!(settle.opened.get(), 1);
    assert!(renderer.settle_follow_band().reconciled);
    assert_live_and_reconciled(&renderer, 8);
    assert_eq!(history_node(&container, 405).text_content(), "after-settle");
}
