//! The held scrollback window: eviction caps the painted rows, every block's
//! placeholder is EXACT, only the open tail opts out of skipping, and the
//! retention-floor marker is paint-only. Ported from
//! `apps/web/tests/renderer/cellRenderer.heldWindow.dom.test.ts` (the bare-length
//! placeholder case lives in `block_placeholder.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    FakeEl, FakeRenderer, PAD_TOP, ROW_PX, delta_frame, mount, row, sb_el, sb_rows,
    seed_held_history, seed_held_history_to, spacer_el,
};
use roost_protocol::cell::{CellGridFrame, CellRow, spans_text};
use roost_web_terminal::presentation::LiveInteractionResult;
use roost_web_terminal::{MAX_HELD_SCROLLBACK_ROWS, ReaderIntent, RenderElement};

const BLOCK: u32 = 250;

fn rows(count: u32, from: u32, prefix: &str) -> Vec<CellRow> {
    (from..from + count)
        .map(|index| row(index, &format!("{prefix}{index}")))
        .collect()
}

fn append_delta(append: Vec<CellRow>, total: u64, seq: u64) -> CellGridFrame {
    CellGridFrame {
        scrollback_total: total,
        ..delta_frame(80, 1, vec![row(0, "v")], append, seq)
    }
}

/// Stream `batches` whole blocks after row `from`; returns the next index.
fn grow(renderer: &mut FakeRenderer, from: u32, batches: u64) -> u32 {
    let mut next = from;
    for batch in 0..batches {
        let append = rows(BLOCK, next, "s");
        next += BLOCK;
        renderer.apply(&append_delta(append, u64::from(next), batch + 3));
    }
    next
}

fn assert_held_window(renderer: &FakeRenderer) {
    let frame = renderer.current_frame().unwrap();
    assert_eq!(
        frame.scrollback_total - frame.sb_base,
        frame.scrollback_rows.len() as u64
    );
    assert!(frame.scrollback_rows.len() <= MAX_HELD_SCROLLBACK_ROWS);
}

#[test]
fn eviction_caps_the_held_window_and_preserves_invariant_and_dom_alignment() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(100, 0, "h"));
    let next = grow(&mut renderer, 100, 12);
    assert_held_window(&renderer);
    let frame = renderer.current_frame().unwrap();
    assert_eq!(sb_rows(&scrollback).len(), frame.scrollback_rows.len());
    let block_cap = MAX_HELD_SCROLLBACK_ROWS.div_ceil(BLOCK as usize) + 1;
    assert!(scrollback.children().len() <= block_cap);
    let tail = frame.scrollback_rows.last().unwrap();
    assert_eq!(spans_text(&tail.spans), format!("s{}", next - 1));
}

#[test]
fn returning_to_the_bottom_reconciles_pending_history_and_re_enables_eviction() {
    let (container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(100, 0, "h"));
    container
        .set_scroll_top_raw(container.scroll_height() - container.client_height() - 3.0 * ROW_PX);
    let unchanged = LiveInteractionResult {
        reconciled: false,
        anchor_changed: false,
    };
    assert_eq!(renderer.handle_scroll(), unchanged);
    container.reset_scroll_top_writes();
    grow(&mut renderer, 100, 8);
    assert_eq!(renderer.reader_intent(), ReaderIntent::Reading);
    assert_eq!(renderer.current_frame().unwrap().scrollback_rows.len(), 100);
    assert_eq!(renderer.canonical_frame_seq(), 10);
    assert_eq!(container.scroll_top_writes(), 0);

    container.set_scroll_top_raw((container.scroll_height() - container.client_height()).max(0.0));
    container.reset_scroll_top_writes();
    let resumed = LiveInteractionResult {
        reconciled: true,
        anchor_changed: true,
    };
    assert_eq!(renderer.handle_scroll(), resumed);
    assert_held_window(&renderer);
    assert_eq!(
        container.scroll_top(),
        container.scroll_height() - container.client_height()
    );
    assert_eq!(container.scroll_top_writes(), 1);
}

#[test]
fn a_partial_leading_history_page_never_desyncs_dom_from_painted_rows() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    let (total, tail_start, chunk) = (2500, 2400, 180);
    seed_held_history_to(
        &mut renderer,
        80,
        vec![row(0, "v")],
        rows(100, tail_start, "h"),
        u64::from(total),
    );
    let page: Vec<CellRow> = (0..chunk)
        .map(|offset| row(tail_start - chunk + offset, &format!("b{offset}")))
        .collect();
    assert!(renderer.insert_history_page(&page, false));
    assert_eq!(
        renderer.painted_scrollback_row_count(),
        (100 + chunk) as usize
    );
    let mut next = total;
    for batch in 0..12 {
        let append = rows(BLOCK, next, "s");
        next += BLOCK;
        renderer.apply(&append_delta(append, u64::from(next), batch + 3));
        assert_held_window(&renderer);
        assert_eq!(
            sb_rows(&scrollback).len(),
            renderer.painted_scrollback_row_count()
        );
    }
}

#[test]
fn only_the_mutable_tail_is_excluded_from_browser_anchoring() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    seed_held_history_to(
        &mut renderer,
        80,
        vec![row(0, "v")],
        rows(300, 300, "h"),
        600,
    );
    let anchoring = |index: usize| scrollback.children()[index].style("overflow-anchor");
    assert_eq!(anchoring(0), None);
    assert_eq!(anchoring(1).as_deref(), Some("none"));
    assert!(renderer.insert_history_page(&[row(299, "backfill")], false));
    assert_eq!(anchoring(2), None);
    renderer.apply(&append_delta(vec![row(600, "stream")], 601, 3));
    assert_eq!(anchoring(2).as_deref(), Some("none"));
}

#[test]
fn explicit_reading_leaves_the_inspected_viewport_and_painted_history_untouched() {
    let (container, mut renderer) = mount();
    container.set_client_height(500.0);
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(2000, 0, "h"));
    container.set_scroll_top_raw(PAD_TOP + 800.0 * ROW_PX);
    renderer.handle_scroll();
    let held = container.scroll_top();
    let base_before = renderer.current_frame().unwrap().sb_base;
    container.reset_scroll_top_writes();
    renderer.apply(&append_delta(
        rows(BLOCK, 2000, "s"),
        u64::from(2000 + BLOCK),
        3,
    ));
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.sb_base, base_before);
    assert_eq!(frame.scrollback_rows.len(), 2000);
    assert_eq!(renderer.canonical_frame_seq(), 3);
    assert_eq!(container.scroll_top(), held);
    assert_eq!(container.scroll_top_writes(), 0);
}

#[test]
fn a_blocks_skipped_state_placeholder_is_its_exact_measured_height_partial_or_full() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(7, 0, "s"));
    let placeholder = |index: usize| scrollback.children()[index].style("contain-intrinsic-size");
    assert_eq!(scrollback.children().len(), 1);
    assert_eq!(placeholder(0).as_deref(), Some("112.00px"));
    renderer.apply(&append_delta(rows(BLOCK, 7, "s"), u64::from(7 + BLOCK), 3));
    assert_eq!(scrollback.children().len(), 2);
    assert_eq!(placeholder(0).as_deref(), Some("4000.00px"));
    assert_eq!(placeholder(1).as_deref(), Some("112.00px"));
}

#[test]
fn only_the_open_tail_block_opts_out_of_content_visibility_and_sealing_restores_it() {
    let (container, mut renderer) = mount();
    let scrollback = sb_el(&container);
    seed_held_history(&mut renderer, 80, vec![row(0, "v")], rows(7, 0, "s"));
    let visibility = |index: usize| scrollback.children()[index].style("content-visibility");
    assert_eq!(visibility(0).as_deref(), Some("visible"));
    renderer.apply(&append_delta(rows(BLOCK, 7, "s"), u64::from(7 + BLOCK), 3));
    assert_eq!(scrollback.children().len(), 2);
    assert_eq!(visibility(0), None);
    assert_eq!(visibility(1).as_deref(), Some("visible"));
}

/// 250 painted history rows above 500 unpainted ones: the spacer is `[0, 500)`.
fn floor_seeded() -> (FakeEl, FakeRenderer, FakeEl) {
    let (container, mut renderer) = mount();
    seed_held_history_to(
        &mut renderer,
        80,
        vec![row(0, "v")],
        rows(250, 500, "s"),
        750,
    );
    container.set_scroll_top_raw(PAD_TOP + 600.0 * ROW_PX);
    container.reset_scroll_top_writes();
    let spacer = spacer_el(&container);
    (container, renderer, spacer)
}

#[test]
fn only_a_floor_the_whole_spacer_sits_below_marks_it_and_never_as_geometry() {
    let (container, mut renderer, spacer) = floor_seeded();
    let geometry = |spacer: &FakeEl| {
        (
            spacer.style("height"),
            container.scroll_top(),
            container.scroll_height(),
        )
    };
    let before = geometry(&spacer);
    assert_eq!(before.0.as_deref(), Some("8000.00px"));
    assert_eq!(spacer.attribute("data-history-floor"), None);
    renderer.set_history_floor(120);
    assert_eq!(spacer.attribute("data-history-floor"), None);
    assert_eq!(geometry(&spacer), before);
    renderer.set_history_floor(500);
    assert_eq!(spacer.attribute("data-history-floor").as_deref(), Some("1"));
    assert_eq!(geometry(&spacer), before);
    renderer.set_history_floor(0);
    assert_eq!(spacer.attribute("data-history-floor"), None);
    assert_eq!(geometry(&spacer), before);
    assert_eq!(container.scroll_top_writes(), 0);
}

#[test]
fn a_splice_that_lowers_the_painted_base_onto_the_floor_marks_the_spacer_itself() {
    let (container, mut renderer, spacer) = floor_seeded();
    let (scroll_top, scroll_height) = (container.scroll_top(), container.scroll_height());
    renderer.set_history_floor(300);
    assert_eq!(spacer.attribute("data-history-floor"), None);
    assert!(renderer.insert_history_page(&rows(200, 300, "s"), false));
    assert_eq!(spacer.attribute("data-history-floor").as_deref(), Some("1"));
    assert_eq!(spacer.style("height").as_deref(), Some("4800.00px"));
    assert_eq!(container.scroll_height(), scroll_height);
    assert_eq!(container.scroll_top(), scroll_top);
    assert_eq!(container.scroll_top_writes(), 0);
}
