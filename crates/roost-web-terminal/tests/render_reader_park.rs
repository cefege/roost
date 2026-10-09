//! Reader parks: an explicit native, selection or wheel park keeps accepted
//! frames — even an incompatible full — off-DOM until its own resume, and a
//! history page never writes the reader's position. Ported from the second
//! half of `apps/web/tests/renderer/cellRenderer.readerIntent.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    FakeEl, PAD_TOP, ROW_PX, alt_delta_frame, alt_full_frame, delta_frame, mount, row, sb_el,
    sb_rows, seed_held_history, seed_held_history_to, vp_rows,
};
use roost_protocol::cell::{CellGridFrame, CellRow, spans_text};
use roost_web_terminal::presentation::{
    LiveInteractionResult, RendererEpochSeq, RendererTerminalModeSnapshot,
};
use roost_web_terminal::{ReaderIntent, ReaderIntentReason, RenderElement};

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

fn next_epoch(seq: u64) -> RendererEpochSeq {
    RendererEpochSeq {
        grid_epoch: Some("test-grid:1".to_string()),
        seq: Some(seq),
    }
}

fn mode(
    alt_screen: bool,
    cursor_keys_app: bool,
    bracketed_paste: bool,
) -> Option<RendererTerminalModeSnapshot> {
    Some(RendererTerminalModeSnapshot {
        alt_screen,
        cursor_keys_app,
        bracketed_paste,
    })
}

#[test]
fn an_incompatible_full_stays_off_dom_during_native_reading_and_reconciles_on_resume() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "old-v")], rows(400, 0));
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    assert_eq!(renderer.handle_scroll(), UNCHANGED);
    let held_row = vp_rows(&container)[0].clone();
    container.reset_scroll_top_writes();

    let modes = |frame: CellGridFrame| CellGridFrame {
        cursor_keys_app: true,
        bracketed_paste: true,
        ..frame
    };
    let full = CellGridFrame {
        scrollback_total: 410,
        sb_base: 410,
        seq: 3,
        ..alt_full_frame(80, vec![row(0, "TUI")])
    };
    assert!(renderer.apply(&modes(full)));
    let delta = CellGridFrame {
        scrollback_append: vec![row(410, "live-410")],
        scrollback_total: 411,
        ..alt_delta_frame(80, 1, vec![row(0, "latest")], 4)
    };
    assert!(renderer.apply(&modes(delta)));

    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(
        renderer.reader_reason(),
        Some(ReaderIntentReason::NativeScroll)
    );
    assert_eq!(renderer.current_frame().unwrap().grid_epoch, "test-grid:0");
    assert_eq!(renderer.canonical_epoch_seq(), next_epoch(4));
    assert_eq!(vp_rows(&container)[0], held_row);
    assert_eq!(container.scroll_top(), PAD_TOP + 50.0 * ROW_PX);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(renderer.backfill_anchor(), None);
    let snapshot = renderer.presentation_snapshot();
    assert_eq!(snapshot.canonical_mode, mode(true, true, true));
    assert_eq!(snapshot.reconciled_mode, mode(false, false, false));

    assert_eq!(renderer.prepare_live_interaction(), RESUMED);
    assert_ne!(vp_rows(&container)[0], held_row);
    let frame = renderer.current_frame().unwrap();
    assert_eq!((frame.grid_epoch.as_str(), frame.seq), ("test-grid:1", 4));
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(
        renderer.reconciled_epoch_seq(),
        renderer.canonical_epoch_seq()
    );
    assert_eq!(
        renderer.presentation_snapshot().reconciled_mode,
        mode(true, true, true)
    );
}

#[test]
fn native_selection_keeps_an_incompatible_full_off_dom_until_selection_release() {
    let (container, mut renderer) = mount();
    seed_held_history(
        &mut renderer,
        80,
        vec![row(0, "selected-old")],
        rows(400, 0),
    );
    let held_row = vp_rows(&container)[0].clone();

    assert_eq!(renderer.set_selection_hold(true), UNCHANGED);
    let full = CellGridFrame {
        seq: 3,
        scrollback_total: 410,
        sb_base: 410,
        ..alt_full_frame(100, vec![row(0, "selected-canonical")])
    };
    assert!(renderer.apply(&full));
    let delta = CellGridFrame {
        scrollback_append: vec![row(410, "live-410")],
        scrollback_total: 411,
        ..alt_delta_frame(100, 1, vec![row(0, "selected-latest")], 4)
    };
    assert!(renderer.apply(&delta));

    assert_eq!(
        renderer.reader_reason(),
        Some(ReaderIntentReason::Selection)
    );
    assert_eq!(renderer.canonical_epoch_seq(), next_epoch(4));
    assert_eq!(renderer.grid_text(), "selected-latest");
    assert_eq!(
        spans_text(&renderer.current_frame().unwrap().viewport_rows[0].spans),
        "selected-old"
    );
    assert_eq!(vp_rows(&container)[0], held_row);

    assert_eq!(renderer.set_selection_hold(false), RESUMED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.seq, 4);
    assert_eq!(spans_text(&frame.viewport_rows[0].spans), "selected-latest");
    assert_ne!(vp_rows(&container)[0], held_row);
    assert_eq!(
        renderer.reconciled_epoch_seq(),
        renderer.canonical_epoch_seq()
    );
}

#[test]
fn selection_release_preserves_an_independent_wheel_reader_interval() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "wheel-old")], rows(400, 0));
    let held_row = vp_rows(&container)[0].clone();
    container.set_scroll_top_raw(bottom_of(&container) - 3.0 * ROW_PX);
    renderer.enter_reading(ReaderIntentReason::Wheel);
    renderer.set_selection_hold(true);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Wheel));

    assert!(renderer.apply(&CellGridFrame {
        seq: 3,
        ..alt_full_frame(80, vec![row(0, "wheel-new")])
    }));
    assert_eq!(renderer.set_selection_hold(false), UNCHANGED);
    assert_eq!(renderer.reader_reason(), Some(ReaderIntentReason::Wheel));
    assert_eq!(vp_rows(&container)[0], held_row);

    assert_eq!(renderer.prepare_live_interaction(), RESUMED);
    assert_ne!(vp_rows(&container)[0], held_row);
}

#[test]
fn a_genuine_return_to_literal_bottom_reconciles_pending_canonical_state() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "old-v")], rows(400, 0));
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    renderer.handle_scroll();
    renderer.apply(&CellGridFrame {
        viewport_rows: vec![row(0, "latest-v")],
        ..append_delta(vec![row(400, "new")], 401, 3)
    });
    assert_eq!(renderer.current_frame().unwrap().seq, 2);

    container.set_scroll_top_raw(bottom_of(&container));
    container.reset_scroll_top_writes();
    assert_eq!(renderer.handle_scroll(), RESUMED);
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.seq, 3);
    assert_eq!(spans_text(&frame.viewport_rows[0].spans), "latest-v");
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(container.scroll_top(), bottom_of(&container));
    assert_eq!(container.scroll_top_writes(), 1);
}

#[test]
fn an_off_bottom_native_reader_returns_to_the_tail_on_explicit_live_preparation() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(400, 0));
    container.set_scroll_top_raw(bottom_of(&container) - 3.0 * ROW_PX);
    renderer.handle_scroll();
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(
        renderer.reader_reason(),
        Some(ReaderIntentReason::NativeScroll)
    );
    renderer.apply(&append_delta(vec![row(400, "new")], 401, 3));

    assert_eq!(renderer.prepare_live_interaction(), RESUMED);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Live);
    assert_eq!(renderer.current_frame().unwrap().seq, 3);
    assert!(renderer.at_bottom());
}

#[test]
fn a_non_bottom_history_page_performs_no_application_scroll_write() {
    let (container, mut renderer) = mount();
    let (total, held) = (1000u32, 300u32);
    seed_held_history_to(
        &mut renderer,
        80,
        vec![row(0, "v")],
        rows(held, total - held),
        u64::from(total),
    );
    container.set_scroll_top_raw(PAD_TOP + 50.0 * ROW_PX);
    let before = container.scroll_top();
    container.reset_scroll_top_writes();

    assert!(renderer.insert_history_page(&rows(100, total - held - 100), false));
    assert_eq!(container.scroll_top(), before);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(
        renderer.backfill_anchor().unwrap().sb_base,
        total - held - 100
    );
    assert_eq!(sb_rows(&sb_el(&container)).len(), (held + 100) as usize);
    assert_eq!(
        renderer.current_frame().unwrap().scrollback_rows.len(),
        held as usize
    );
}
