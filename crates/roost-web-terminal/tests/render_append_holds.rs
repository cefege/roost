//! Paint holds on the append-only grid: a selection or armed-link hold freezes
//! the painted DOM while the canonical frame keeps advancing, and the release
//! is the resume a park that lost every other trigger still gets. Ported from
//! the hold cases of `apps/web/tests/renderer/cellRenderer.append.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    FakeEl, FakeRenderer, ROW_PX, alt_full_frame, delta_frame, mount, numbered_rows, row, sb_el,
    sb_rows, seed_held_history, vp_el,
};
use roost_protocol::cell::CellGridFrame;
use roost_web_terminal::presentation::{LiveInteractionResult, RendererEpochSeq};
use roost_web_terminal::{
    RENDERER_HOLD_LINK, RENDERER_HOLD_SELECTION, ReaderIntent, ReaderIntentReason,
    ReconcileBlockReason, RenderElement,
};

const UNCHANGED: LiveInteractionResult = LiveInteractionResult {
    reconciled: false,
    anchor_changed: false,
};
const RESUMED: LiveInteractionResult = LiveInteractionResult {
    reconciled: true,
    anchor_changed: true,
};

fn epoch_seq(seq: u64) -> RendererEpochSeq {
    RendererEpochSeq {
        grid_epoch: Some("test-grid:0".to_string()),
        seq: Some(seq),
    }
}

fn viewport_delta(text: &str, seq: u64, total: u64) -> CellGridFrame {
    CellGridFrame {
        scrollback_total: total,
        ..delta_frame(80, 1, vec![row(0, text)], Vec::new(), seq)
    }
}

/// 400 history rows under one viewport row; returns the painted viewport row.
fn deep_pane() -> (FakeEl, FakeRenderer, FakeEl) {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v0")], numbered_rows(400, 0));
    let held_row = vp_el(&container).children()[0].clone();
    (container, renderer, held_row)
}

#[test]
fn selection_freezes_canonical_paint_and_reconciles_when_its_last_hold_releases() {
    let (container, mut renderer) = mount();
    let (viewport, scrollback) = (vp_el(&container), sb_el(&container));
    assert!(seed_held_history(
        &mut renderer,
        80,
        vec![row(0, "v0")],
        vec![row(0, "h0")]
    ));
    let before_hold = renderer.reconciled_epoch_seq();
    let held_row = viewport.children()[0].clone();

    assert_eq!(renderer.set_selection_hold(true), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.hold_mask(), RENDERER_HOLD_SELECTION);
    let appended = CellGridFrame {
        scrollback_total: 2,
        ..delta_frame(80, 1, vec![row(0, "v0-changed")], vec![row(1, "h1")], 3)
    };
    assert!(renderer.apply(&appended));
    assert!(renderer.apply(&viewport_delta("v0-again", 4, 2)));
    assert_eq!(viewport.children()[0], held_row);
    assert_eq!(sb_rows(&scrollback).len(), 1);
    assert_eq!(renderer.canonical_epoch_seq(), epoch_seq(4));
    assert_eq!(renderer.reconciled_epoch_seq(), before_hold);
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::ReaderPendingFrame
    );
    let snapshot = renderer.presentation_snapshot();
    assert_eq!(
        (snapshot.hold_mask_selection, snapshot.hold_mask_link),
        (true, false)
    );
    assert_eq!(renderer.grid_text(), "v0-again");

    assert_eq!(renderer.set_selection_hold(false), RESUMED);
    assert_ne!(viewport.children()[0], held_row);
    assert_eq!(sb_rows(&scrollback).len(), 2);
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(4));
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn an_armed_link_hold_freezes_the_viewport_and_flushes_on_release_without_entering_reading() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, vec![row(0, "v0")], Vec::new());
    let held_row = viewport.children()[0].clone();

    renderer.set_armed_hold(true);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.hold_mask(), RENDERER_HOLD_LINK);
    renderer.apply(&delta_frame(
        80,
        1,
        vec![row(0, "v0-changed")],
        Vec::new(),
        2,
    ));
    assert_eq!(viewport.children()[0], held_row);
    assert_eq!(renderer.canonical_epoch_seq(), epoch_seq(2));
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(1));
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::LinkHold
    );

    let flushed = LiveInteractionResult {
        reconciled: true,
        anchor_changed: false,
    };
    assert_eq!(renderer.set_armed_hold(false), flushed);
    assert_ne!(viewport.children()[0], held_row);
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(2));
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn a_hold_release_resumes_a_wheel_park_whose_box_lost_its_scroll_range() {
    let (container, mut renderer, held_row) = deep_pane();
    container.set_scroll_top_raw(0.0);
    renderer.enter_reading(ReaderIntentReason::Wheel);
    renderer.set_armed_hold(true);
    assert!(renderer.apply(&viewport_delta("v0-latest", 3, 400)));

    container.set_client_height(container.scroll_height() + ROW_PX);
    assert_eq!(renderer.note_box_resize(), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.note_box_resize(), UNCHANGED);

    assert!(renderer.set_armed_hold(false).reconciled);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_ne!(vp_el(&container).children()[0], held_row);
    assert_eq!(vp_el(&container).text_content(), "v0-latest");
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn a_hold_release_resumes_a_bottom_following_wheel_park_that_kept_its_range() {
    let (container, mut renderer, held_row) = deep_pane();
    renderer.enter_reading(ReaderIntentReason::Wheel);
    renderer.set_armed_hold(true);
    container.set_scroll_top_raw(container.scroll_height() - container.client_height());
    assert!(renderer.apply(&viewport_delta("v0-latest", 3, 400)));
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);

    assert!(renderer.set_armed_hold(false).reconciled);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_ne!(vp_el(&container).children()[0], held_row);
    assert_eq!(vp_el(&container).text_content(), "v0-latest");
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn a_hold_release_leaves_a_find_park_that_can_still_reach_its_anchor() {
    let (container, mut renderer, held_row) = deep_pane();
    assert!(renderer.scroll_to_scrollback_row(50));
    renderer.handle_scroll();
    renderer.set_armed_hold(true);
    assert!(renderer.apply(&viewport_delta("v0-latest", 3, 400)));

    assert_eq!(renderer.set_armed_hold(false), UNCHANGED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Find));
    assert_eq!(vp_el(&container).children()[0], held_row);
}

#[test]
fn selection_and_link_holds_clear_atomically_with_at_most_one_epoch_repair() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, vec![row(0, "v0")], Vec::new());
    renderer.set_selection_hold(true);
    renderer.set_armed_hold(true);
    assert_eq!(
        renderer.hold_mask(),
        RENDERER_HOLD_SELECTION | RENDERER_HOLD_LINK
    );
    let snapshot = renderer.presentation_snapshot();
    assert_eq!(
        (snapshot.hold_mask_selection, snapshot.hold_mask_link),
        (true, true)
    );
    assert!(renderer.apply(&CellGridFrame {
        seq: 2,
        ..alt_full_frame(80, vec![row(0, "TUI")])
    }));

    let wipes_before = viewport.clear_children_calls();
    assert_eq!(renderer.prepare_live_interaction(), RESUMED);
    assert_eq!(viewport.clear_children_calls() - wipes_before, 1);
    assert_eq!(renderer.hold_mask(), 0);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert!(
        container
            .class_name()
            .split_whitespace()
            .any(|class| class == "alt-active")
    );
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}
