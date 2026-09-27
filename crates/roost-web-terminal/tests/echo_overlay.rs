//! The painted half of predictive local echo, at the only level that can be
//! checked without a browser: the plan the overlay stamps and the caret the
//! renderer's watermark is measured against.
//!
//! Ported from `apps/web/tests/renderer/cellRenderer.reconcile.dom.test.ts`
//! ("a leading predicted caret does not freeze reconciliation") and the
//! overlay-cell assertions of `apps/web/tests/predictiveEchoAck.test.ts`, which
//! read a fake DOM. The DOM adapter itself is `wasm32`-only and is proven by the
//! Playwright oracle, so what is left here is the arithmetic it stamps and the
//! claim that a predicted caret is not a reconcile block.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::predictive_echo::{EchoPaint, PredictedCell};
use roost_web_terminal::echo_overlay::{
    PREDICTED_ERASE_CLASS, PREDICTED_GLYPH_CLASS, cell_left, cell_top, plan_paint, prediction_style,
};
use roost_web_terminal::reader_intent::{ReaderState, ReconcileBlockReason};

/// A paint request over one row: `ch` empty is an erase cell.
fn paint(cells: &[(u32, &str)], flagged: bool, caret_col: Option<u32>) -> EchoPaint {
    EchoPaint {
        cells: cells
            .iter()
            .map(|(col, ch)| PredictedCell {
                row: 0,
                col: *col,
                ch: (*ch).to_string(),
            })
            .collect(),
        flagged,
        caret_col,
    }
}

#[test]
fn a_predicted_cell_is_placed_by_grid_units_not_pixels() {
    let planned = plan_paint(&paint(&[(7, "x")], false, None));
    assert_eq!(planned.len(), 1);
    assert_eq!(planned[0].left, "7ch");
    assert_eq!(planned[0].top, "0lh");
    assert_eq!(planned[0].class_name, PREDICTED_GLYPH_CLASS);
    assert_eq!(planned[0].text, "x");
    assert_eq!(
        prediction_style(&planned[0]),
        "position:absolute;top:0lh;left:7ch"
    );
}

#[test]
fn an_erase_cell_paints_no_glyph_and_is_never_flagged() {
    let planned = plan_paint(&paint(&[(1, ""), (2, "z")], true, None));
    assert_eq!(planned[0].class_name, PREDICTED_ERASE_CLASS);
    assert_eq!(planned[0].text, "");
    assert!(
        !planned[0].underlined,
        "an underline would draw a line that is not there"
    );
    // A flagged GLYPH is underlined, so the user can tell a guess from an echo.
    assert!(planned[1].underlined);
    assert_eq!(planned[1].class_name, PREDICTED_GLYPH_CLASS);
    assert!(prediction_style(&planned[1]).ends_with(";text-decoration:underline"));
    assert!(!prediction_style(&planned[0]).contains("text-decoration"));
}

#[test]
fn an_empty_plan_paints_nothing() {
    assert!(plan_paint(&paint(&[], false, None)).is_empty());
}

#[test]
fn every_planned_cell_sits_on_its_own_grid_offset() {
    let planned = plan_paint(&paint(&[(0, "a"), (1, "b"), (2, "c")], false, Some(3)));
    let offsets: Vec<(String, String)> = planned
        .iter()
        .map(|cell| (cell.top.clone(), cell.left.clone()))
        .collect();
    assert_eq!(
        offsets,
        vec![
            ("0lh".to_string(), "0ch".to_string()),
            ("0lh".to_string(), "1ch".to_string()),
            ("0lh".to_string(), "2ch".to_string()),
        ]
    );
    assert_eq!(cell_top(0), "0lh");
    assert_eq!(cell_left(3), "3ch");
}

/// A leading predicted caret is a CLIENT OVERLAY, and a client overlay never
/// blocks DOM reconciliation. The pane's watchdog reads the block reason, so a
/// predicted caret that appeared there would freeze the watermark for the whole
/// length of a typing burst and the watchdog would wipe correct predictions once
/// a second — `docs/FAILURE-INDEX.md:2422`. There is no parameter through which
/// a prediction could reach the decision at all, and every reason it can return
/// names a real DOM-fidelity condition.
#[test]
fn a_leading_predicted_caret_does_not_freeze_reconciliation() {
    let plan = paint(&[(0, "a"), (1, "b")], false, Some(7));
    assert_eq!(
        plan.caret_col,
        Some(7),
        "the caret leads the two painted glyphs and the authoritative column"
    );

    let reader = ReaderState::new();
    let frame_watermark = (Some("echo-grid:0"), Some(9));
    assert_eq!(
        reader.reconcile_block_reason(false, false, frame_watermark, frame_watermark),
        ReconcileBlockReason::None,
        "an otherwise-current frame is not blocked by anything the overlay did"
    );

    // A selection hold and a pending render DO block, so the `None` above is a
    // real verdict about the DOM rather than a reason that never fires. A
    // parked reader is deliberately NOT one of them: parking is a reader
    // position, and it blocks by not being read, not through this reason.
    let mut held = ReaderState::new();
    held.set_selection_hold(true);
    let pending = ReaderState::new();
    for (blocking, reason) in [
        (
            held.reconcile_block_reason(false, false, frame_watermark, frame_watermark),
            ReconcileBlockReason::SelectionHold,
        ),
        (
            pending.reconcile_block_reason(false, true, frame_watermark, frame_watermark),
            ReconcileBlockReason::PendingRender,
        ),
    ] {
        assert_eq!(
            blocking, reason,
            "a real DOM-fidelity block still reports itself"
        );
    }
}
