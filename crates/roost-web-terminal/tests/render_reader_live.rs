//! Reader intent on the live tail: live output follows the tail and its owned
//! scroll event stays live, a coalesced or clamped pin leaves no stale scroll
//! ownership, and only a genuine gesture reads. Ported from the first half of
//! `apps/web/tests/renderer/cellRenderer.readerIntent.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{FakeEl, FakeRenderer, ROW_PX, delta_frame, mount, row, seed_held_history};
use roost_protocol::cell::{CellGridFrame, CellRow};
use roost_web_terminal::presentation::LiveInteractionResult;
use roost_web_terminal::{ReaderIntent, ReaderIntentReason, RenderElement};

const UNCHANGED: LiveInteractionResult = LiveInteractionResult {
    reconciled: false,
    anchor_changed: false,
};
const RECONCILED_IN_PLACE: LiveInteractionResult = LiveInteractionResult {
    reconciled: true,
    anchor_changed: false,
};

fn rows(count: u32, from: u32) -> Vec<CellRow> {
    (from..from + count)
        .map(|index| row(index, &format!("s{index}")))
        .collect()
}

fn append_delta(append: Vec<CellRow>, total: u64, seq: u64) -> CellGridFrame {
    CellGridFrame {
        scrollback_total: total,
        ..delta_frame(80, 1, vec![row(0, "v")], append, seq)
    }
}

fn bottom_of(container: &FakeEl) -> f64 {
    (container.scroll_height() - container.client_height()).max(0.0)
}

/// 400 history rows; the reader observed at the literal bottom.
fn at_bottom_pane() -> (FakeEl, FakeRenderer, f64) {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(400, 0));
    let bottom = bottom_of(&container);
    container.set_scroll_top_raw(bottom);
    renderer.handle_scroll();
    (container, renderer, bottom)
}

fn assert_reading(renderer: &FakeRenderer, reason: ReaderIntentReason) {
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.reader_reason(), Some(reason));
}

#[test]
fn live_output_follows_the_tail_and_its_owned_scroll_event_stays_live() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(400, 0));
    container.set_scroll_top_raw(bottom_of(&container));
    container.reset_scroll_top_writes();
    renderer.apply(&append_delta(vec![row(400, "new")], 401, 3));
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
}

#[test]
fn a_coalesced_pin_retargets_once_then_the_next_native_scroll_reads() {
    let (container, mut renderer, bottom) = at_bottom_pane();
    renderer.set_selection_hold(true);
    container.set_scroll_top_raw(bottom - 1.0);
    assert!(renderer.apply(&CellGridFrame {
        cursor_col: 1,
        ..append_delta(Vec::new(), 400, 3)
    }));
    container.reset_scroll_top_writes();
    assert_eq!(renderer.prepare_live_interaction(), RECONCILED_IN_PLACE);
    assert_eq!(container.scroll_top_writes(), 1);

    // Layout clamps the position before the event lands; a second live pin is
    // already unchanged at that final coalesced value.
    container.set_client_height(container.client_height() + ROW_PX);
    let final_owned_top = bottom_of(&container);
    container.set_scroll_top_raw(final_owned_top);
    container.reset_scroll_top_writes();
    assert_eq!(renderer.prepare_live_interaction(), UNCHANGED);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(
        renderer.canonical_epoch_seq(),
        renderer.reconciled_epoch_seq()
    );
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);

    container.set_client_height(container.client_height() - 3.0 * ROW_PX);
    assert!(!renderer.at_bottom());
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);

    container.set_scroll_top_raw(final_owned_top - ROW_PX);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_reading(&renderer, ReaderIntentReason::NativeScroll);
}

#[test]
fn selection_release_re_pins_before_its_owned_event_then_the_next_wheel_reads() {
    let (container, mut renderer, bottom) = at_bottom_pane();
    renderer.set_selection_hold(true);
    assert!(renderer.apply(&CellGridFrame {
        cursor_col: 1,
        ..append_delta(Vec::new(), 400, 3)
    }));
    assert_eq!(renderer.prepare_live_interaction(), RECONCILED_IN_PLACE);
    renderer.begin_live_selection_release();
    container.set_scroll_top_raw(0.0);
    container.reset_scroll_top_writes();

    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(container.scroll_top(), bottom);
    assert_eq!(container.scroll_top_writes(), 1);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.reader_reason(), None);
    assert_eq!(renderer.hold_mask(), 0);
    assert_eq!(
        renderer.canonical_epoch_seq(),
        renderer.reconciled_epoch_seq()
    );

    renderer.finish_live_selection_release();
    container.reset_scroll_top_writes();
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);

    renderer.enter_reading(ReaderIntentReason::Wheel);
    container.set_scroll_top_raw(bottom - ROW_PX);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    assert_reading(&renderer, ReaderIntentReason::Wheel);
}

#[test]
fn unchanged_and_fully_clamped_pins_leave_no_stale_scroll_ownership() {
    let (container, mut renderer, bottom) = at_bottom_pane();
    container.set_scroll_top_raw(bottom - 3.0 * ROW_PX);
    renderer.handle_scroll();

    let clamped_top = container.scroll_top();
    container.set_next_scroll_top_write_result(Some(clamped_top));
    assert_eq!(renderer.prepare_live_interaction(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert!(!renderer.at_bottom());
    renderer.handle_scroll();
    assert_reading(&renderer, ReaderIntentReason::NativeScroll);

    container.set_scroll_top_raw(bottom);
    renderer.handle_scroll();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    container.set_client_height(container.client_height() - 3.0 * ROW_PX);
    assert!(!renderer.at_bottom());
    renderer.handle_scroll();
    assert_reading(&renderer, ReaderIntentReason::NativeScroll);
}

#[test]
fn an_unobserved_off_bottom_position_stays_unmoved_despite_live_reader_intent() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(400, 0));
    let before = bottom_of(&container) - 3.0 * ROW_PX;
    container.set_scroll_top_raw(before);
    container.reset_scroll_top_writes();
    renderer.apply(&append_delta(vec![row(400, "new")], 401, 3));
    assert_eq!(container.scroll_top(), before);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
}

#[test]
fn a_live_reader_inside_the_follow_band_keeps_following_the_tail() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(400, 0));
    container.set_scroll_top_raw(bottom_of(&container) - ROW_PX);
    renderer.handle_scroll();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    container.reset_scroll_top_writes();
    renderer.apply(&append_delta(vec![row(400, "new")], 401, 3));
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
}

#[test]
fn live_intent_persists_through_box_changes_before_the_resize_observer_runs() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(400, 0));
    container.set_scroll_top_raw(bottom_of(&container));
    container.reset_scroll_top_writes();
    container.set_client_height(400.0);
    for step in 0..3u32 {
        let append = rows(50, 400 + 50 * step);
        renderer.apply(&append_delta(
            append,
            u64::from(450 + 50 * step),
            u64::from(3 + step),
        ));
    }
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert!(renderer.at_bottom());
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(container.scroll_top_writes(), 3);
}
