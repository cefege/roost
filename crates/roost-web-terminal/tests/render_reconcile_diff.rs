//! Viewport reconciliation restraint: a delta touches only the rows the worker
//! marked dirty, a scroll reuses shifted row nodes, a held batch leaves the DOM
//! alone until release, and a partial-region scroll keeps its fixed panel.
//! Ported from the viewport-diff cases of
//! `apps/web/tests/renderer/cellRenderer.reconcile.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use std::cell::Cell;
use std::rc::Rc;

use render_support::{
    FakeEl, delta_frame, full_frame, mount, row, sb_el, sb_rows, seed_held_history, vp_el,
};
use roost_protocol::cell::{CellGridFrame, CellRow};
use roost_web_terminal::presentation::{
    LiveInteractionResult, RendererEpochSeq, RendererTerminalModeSnapshot,
};
use roost_web_terminal::{
    CellGridRenderer, ReaderIntentReason, ReconcileBlockReason, RenderElement,
};

const IN_PLACE: LiveInteractionResult = LiveInteractionResult {
    reconciled: true,
    anchor_changed: false,
};

fn epoch_seq(seq: u64) -> RendererEpochSeq {
    RendererEpochSeq {
        grid_epoch: Some("test-grid:0".to_string()),
        seq: Some(seq),
    }
}

fn rows(texts: &[&str]) -> Vec<CellRow> {
    texts
        .iter()
        .enumerate()
        .map(|(index, text)| row(index as u32, text))
        .collect()
}

fn child(viewport: &FakeEl, index: usize) -> FakeEl {
    viewport.children()[index].clone()
}

fn cursor_of(viewport: &FakeEl) -> FakeEl {
    viewport
        .children()
        .into_iter()
        .find(|element| element.class_name() == "cell-cursor")
        .unwrap()
}

/// `(canonical (visible,row,col), painted (visible,row,col), connected)`.
fn cursor_snapshot(
    renderer: &render_support::FakeRenderer,
) -> (
    Option<(bool, u32, u32)>,
    (Option<bool>, Option<i64>, Option<i64>),
    bool,
) {
    let snapshot = renderer.presentation_snapshot();
    let painted = (
        snapshot.painted_cursor_visible,
        snapshot.painted_cursor_row,
        snapshot.painted_cursor_col,
    );
    (
        snapshot.canonical_cursor,
        painted,
        snapshot.cursor_connected,
    )
}

fn cursor_delta(row_index: u32, column: u32) -> CellGridFrame {
    CellGridFrame {
        cursor_row: row_index,
        cursor_col: column,
        ..delta_frame(80, 2, Vec::new(), Vec::new(), 2)
    }
}

#[test]
fn a_content_identical_delta_advances_reconciliation_without_replacing_rows() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["v0", "v1"]), Vec::new());
    let (first, second) = (child(&viewport, 0), child(&viewport, 1));
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(1));
    renderer.apply(&delta_frame(80, 2, rows(&["v0", "v1"]), Vec::new(), 2));
    assert_eq!((child(&viewport, 0), child(&viewport, 1)), (first, second));
    assert_eq!(renderer.canonical_epoch_seq(), epoch_seq(2));
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(2));
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn an_empty_row_cursor_only_delta_moves_the_cursor_and_preserves_every_row_node() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["v0", "v1"]), Vec::new());
    let (first, second) = (child(&viewport, 0), child(&viewport, 1));
    let cursor = cursor_of(&viewport);
    assert!(renderer.apply_delta_frames(&[cursor_delta(1, 3)]));
    assert_eq!((child(&viewport, 0), child(&viewport, 1)), (first, second));
    assert_eq!(cursor.style("top").as_deref(), Some("1lh"));
    assert_eq!(cursor.style("left").as_deref(), Some("3ch"));
    let data = |name: &str| cursor.attribute(name);
    assert_eq!(
        (data("data-row"), data("data-column"), data("data-visible")),
        (
            Some("1".to_string()),
            Some("3".to_string()),
            Some("true".to_string())
        )
    );
    assert_eq!(
        cursor_snapshot(&renderer),
        (Some((true, 1, 3)), (Some(true), Some(1), Some(3)), true)
    );
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(2));
}

#[test]
fn a_leading_predicted_caret_does_not_freeze_reconciliation() {
    let (_container, mut renderer) = mount();
    seed_held_history(&mut renderer, 80, rows(&["v0", "v1"]), Vec::new());
    renderer.set_predicted_cursor(Some(7));
    assert!(renderer.apply_delta_frames(&[cursor_delta(1, 3)]));
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(2));
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn cursor_only_pending_state_resumes_cleanly_without_replacing_row_nodes() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["v0", "v1"]), Vec::new());
    let (first, second) = (child(&viewport, 0), child(&viewport, 1));
    renderer.enter_reading(ReaderIntentReason::Wheel);
    assert!(renderer.apply_delta_frames(&[cursor_delta(1, 4)]));
    assert_eq!(
        cursor_snapshot(&renderer),
        (Some((true, 1, 4)), (Some(true), Some(0), Some(0)), true)
    );
    assert_eq!(renderer.prepare_live_interaction(), IN_PLACE);
    assert_eq!((child(&viewport, 0), child(&viewport, 1)), (first, second));
    assert_eq!(
        cursor_snapshot(&renderer),
        (Some((true, 1, 4)), (Some(true), Some(1), Some(4)), true)
    );
}

#[test]
fn a_one_row_delta_replaces_only_that_rows_node_positionally() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["v0", "v1"]), Vec::new());
    let (first, second) = (child(&viewport, 0), child(&viewport, 1));
    renderer.apply(&delta_frame(
        80,
        2,
        vec![row(1, "v1-changed")],
        Vec::new(),
        2,
    ));
    assert_eq!(child(&viewport, 0), first);
    assert_ne!(child(&viewport, 1), second);
    assert_eq!(child(&viewport, 1).text_content(), "v1-changed");
}

#[test]
fn a_viewport_only_full_frame_rebuild_prunes_surplus_viewport_rows() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["v0", "v1", "v2"]), Vec::new());
    let first = child(&viewport, 0);
    assert_eq!(viewport.children().len(), 5);
    seed_held_history(&mut renderer, 80, rows(&["v0"]), Vec::new());
    assert_eq!(viewport.children().len(), 3);
    assert_ne!(child(&viewport, 0), first);
}

#[test]
fn a_compatible_full_repairs_only_changed_viewport_rows() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["stable", "old"]), Vec::new());
    let (stable, old) = (child(&viewport, 0), child(&viewport, 1));
    assert!(renderer.apply_full_frame(&CellGridFrame {
        seq: 2,
        ..full_frame(80, rows(&["stable", "new"]), 0)
    }));
    assert_eq!(child(&viewport, 0), stable);
    assert_ne!(child(&viewport, 1), old);
    assert_eq!(child(&viewport, 1).text_content(), "new");
}

#[test]
fn a_scrolling_delta_reuses_shifted_row_nodes_and_only_the_new_tail_renders() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["A", "B", "C"]), Vec::new());
    let (node_b, node_c) = (child(&viewport, 1), child(&viewport, 2));
    let scrolled = CellGridFrame {
        scrollback_total: 1,
        ..delta_frame(80, 3, rows(&["B", "C", "D"]), vec![row(0, "A")], 2)
    };
    assert!(renderer.apply_delta_frames(&[scrolled]));
    assert_eq!(
        (child(&viewport, 0), child(&viewport, 1)),
        (node_b, node_c.clone())
    );
    assert_ne!(child(&viewport, 2), node_c);
    assert_eq!(child(&viewport, 2).text_content(), "D");
    assert_eq!(renderer.grid_text(), "B\nC\nD");
}

#[test]
fn a_contiguous_scrolling_batch_shifts_once_and_reconciles_final_modes() {
    let container = FakeEl::container();
    let reconciliations = Rc::new(Cell::new(0u32));
    let counter = reconciliations.clone();
    let on_reconcile: Box<dyn Fn()> = Box::new(move || counter.set(counter.get() + 1));
    let mut renderer =
        CellGridRenderer::with_callbacks(&container, None, Some(on_reconcile), None).unwrap();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["A", "B", "C"]), Vec::new());
    let node_c = child(&viewport, 2);
    reconciliations.set(0);
    let first = CellGridFrame {
        scrollback_total: 1,
        ..delta_frame(80, 3, rows(&["B", "C", "D"]), vec![row(0, "A")], 2)
    };
    let second = CellGridFrame {
        scrollback_total: 2,
        cursor_row: 2,
        cursor_col: 3,
        cursor_keys_app: true,
        bracketed_paste: true,
        ..delta_frame(80, 3, rows(&["C", "D", "E"]), vec![row(1, "B")], 3)
    };
    assert!(renderer.apply_delta_frames(&[first, second]));
    assert_eq!(reconciliations.get(), 1);
    assert_eq!(child(&viewport, 0), node_c);
    assert_eq!(child(&viewport, 1).text_content(), "D");
    assert_eq!(child(&viewport, 2).text_content(), "E");
    assert_eq!(renderer.grid_text(), "C\nD\nE");
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(3));
    let modes = Some(RendererTerminalModeSnapshot {
        alt_screen: false,
        cursor_keys_app: true,
        bracketed_paste: true,
    });
    let snapshot = renderer.presentation_snapshot();
    assert_eq!(
        (snapshot.canonical_mode, snapshot.reconciled_mode),
        (modes, modes)
    );
}

#[test]
fn a_held_batch_preserves_dom_until_explicit_release() {
    let (container, mut renderer) = mount();
    let viewport = vp_el(&container);
    seed_held_history(&mut renderer, 80, rows(&["A", "B"]), Vec::new());
    let (node_a, node_b) = (child(&viewport, 0), child(&viewport, 1));
    renderer.enter_reading(ReaderIntentReason::Wheel);
    let batch = [
        delta_frame(80, 2, vec![row(1, "B1")], Vec::new(), 2),
        delta_frame(80, 2, vec![row(1, "B2")], Vec::new(), 3),
    ];
    assert!(renderer.apply_delta_frames(&batch));
    assert_eq!(
        (child(&viewport, 0), child(&viewport, 1)),
        (node_a.clone(), node_b.clone())
    );
    assert_eq!(renderer.canonical_epoch_seq(), epoch_seq(3));
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq(1));
    assert_eq!(renderer.prepare_live_interaction(), IN_PLACE);
    assert_eq!(child(&viewport, 0), node_a);
    assert_ne!(child(&viewport, 1), node_b);
    assert_eq!(child(&viewport, 1).text_content(), "B2");
}

fn fixed_panel_pane() -> (FakeEl, render_support::FakeRenderer, FakeEl) {
    let (container, mut renderer) = mount();
    let panel = [
        "HEAD-0",
        "GREP-HEAD",
        "README.md#8C59",
        "BODY-A",
        "FIXED-PANEL",
        "STATUS-000",
    ];
    seed_held_history(&mut renderer, 80, rows(&panel), Vec::new());
    let fixed = child(&vp_el(&container), 4);
    (container, renderer, fixed)
}

fn region_scroll() -> CellGridFrame {
    let dirty = vec![
        row(0, "GREP-HEAD"),
        row(1, "README.md#8C59"),
        row(2, "BODY-A"),
        row(3, "NEXT"),
        row(5, "STATUS-001"),
    ];
    CellGridFrame {
        scrollback_total: 1,
        ..delta_frame(80, 6, dirty, vec![row(0, "HEAD-0")], 2)
    }
}

fn assert_fixed_panel_kept(container: &FakeEl, fixed: &FakeEl, status: &str) {
    let viewport = vp_el(container);
    let texts: Vec<String> = viewport.children()[..6]
        .iter()
        .map(FakeEl::text_content)
        .collect();
    assert_eq!(
        texts,
        [
            "GREP-HEAD",
            "README.md#8C59",
            "BODY-A",
            "NEXT",
            "FIXED-PANEL",
            status
        ]
    );
    assert_eq!(child(&viewport, 4), *fixed);
    let history: Vec<String> = sb_rows(&sb_el(container))
        .iter()
        .map(FakeEl::text_content)
        .collect();
    assert_eq!(history, ["HEAD-0"]);
}

#[test]
fn a_partial_region_scroll_retains_the_fixed_panel_and_worker_history() {
    let (container, mut renderer, fixed) = fixed_panel_pane();
    assert!(renderer.apply_delta_frames(&[region_scroll()]));
    assert_fixed_panel_kept(&container, &fixed, "STATUS-001");
}

#[test]
fn a_batched_partial_region_scroll_retains_the_fixed_panel_and_latest_status() {
    let (container, mut renderer, fixed) = fixed_panel_pane();
    let status = CellGridFrame {
        scrollback_total: 1,
        ..delta_frame(80, 6, vec![row(5, "STATUS-002")], Vec::new(), 3)
    };
    assert!(renderer.apply_delta_frames(&[region_scroll(), status]));
    assert_fixed_panel_kept(&container, &fixed, "STATUS-002");
}
