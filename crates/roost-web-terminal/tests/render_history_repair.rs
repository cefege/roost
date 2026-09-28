//! A live repair or renewal never replaces painted history: a viewport-only
//! full keeps fetched rows and inserts only the missing tail gap, an
//! incompatible full keeps the reader window until explicit resume, and
//! releasing selection reconciles pending canonical state. Ported from
//! `apps/web/tests/renderer/cellRenderer.history.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_history_support;
mod render_support;

use render_history_support::{
    RECONCILED_WITH_NEW_ANCHOR, at_reader_row, epoch_seq, row_at_reader, seeded,
};
use render_support::{
    PAD_TOP, ROW_PX, delta_frame, full_frame, numbered_rows, row, sb_el, sb_rows,
};
use roost_protocol::cell::{CellGridFrame, spans_text};
use roost_web_terminal::presentation::PaintedRowText;
use roost_web_terminal::{ReaderAnchor, ReconcileBlockReason, RenderElement};

#[test]
fn a_live_viewport_only_full_preserves_painted_history_and_inserts_only_the_missing_tail_gap() {
    let (container, mut renderer) = seeded("old-v");
    let history_nodes = sb_rows(&sb_el(&container));
    let repair = CellGridFrame {
        seq: 3,
        ..full_frame(80, vec![row(0, "repair-v")], 760)
    };
    assert!(renderer.apply_full_frame(&repair));
    let frame = renderer.current_frame().unwrap();
    assert_eq!((frame.sb_base, frame.scrollback_rows.len()), (760, 0));
    let anchor = renderer.backfill_anchor().unwrap();
    assert_eq!((anchor.sb_base, anchor.total), (500, 760));
    assert_eq!(sb_rows(&sb_el(&container)), history_nodes);
    assert_eq!(renderer.grid_text(), "repair-v");
    let presentation = renderer.paint_presentation(None);
    let expected: Vec<PaintedRowText> = numbered_rows(250, 500)
        .iter()
        .map(|entry| PaintedRowText {
            index: entry.index,
            text: spans_text(&entry.spans),
        })
        .collect();
    assert_eq!(presentation.rows, expected);
    assert_eq!(presentation.head_spacer_px, 500.0 * ROW_PX);
    assert_eq!(presentation.tail_gap_px, 10.0 * ROW_PX);
    assert_eq!(presentation.reader_anchor, None);

    let live = |text: &str, index: u32, seq: u64| CellGridFrame {
        scrollback_total: u64::from(index) + 1,
        ..delta_frame(
            80,
            1,
            vec![row(0, text)],
            vec![row(index, &format!("live-{index}"))],
            seq,
        )
    };
    assert!(renderer.apply(&live("delta-1", 760, 4)));
    assert!(renderer.apply(&live("delta-2", 761, 5)));
    assert_eq!(
        sb_rows(&sb_el(&container))[..history_nodes.len()],
        history_nodes[..]
    );
    let indices: Vec<u32> = renderer
        .current_frame()
        .unwrap()
        .scrollback_rows
        .iter()
        .map(|entry| entry.index)
        .collect();
    assert_eq!(indices[indices.len() - 2..], [760, 761]);
    assert_eq!(renderer.paint_presentation(None).tail_gap_px, 10.0 * ROW_PX);
    assert_eq!(renderer.grid_text(), "delta-2");
}

#[test]
fn a_same_epoch_streaming_repair_preserves_fetched_history_through_explicit_reading_and_atomic_resume()
 {
    let (container, mut renderer) = seeded("old-v");
    let history_nodes = sb_rows(&sb_el(&container));
    let reconciled_before = renderer.reconciled_epoch_seq();
    at_reader_row(&container, &mut renderer, 600.0);
    let reader_before = row_at_reader(&container);
    let height_before = container.scroll_height();
    container.reset_scroll_top_writes();

    renderer.apply(&CellGridFrame {
        seq: 3,
        ..full_frame(80, vec![row(0, "repair-v")], 760)
    });
    renderer.apply(&CellGridFrame {
        scrollback_total: 761,
        ..delta_frame(
            80,
            1,
            vec![row(0, "latest-v")],
            vec![row(760, "live-760")],
            4,
        )
    });
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.scrollback_total, 750);
    assert_eq!(spans_text(&frame.viewport_rows[0].spans), "old-v");
    assert_eq!(renderer.canonical_frame_seq(), 4);
    assert_eq!(renderer.canonical_epoch_seq(), epoch_seq("test-grid:0", 4));
    assert_eq!(renderer.reconciled_epoch_seq(), reconciled_before);
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::ReaderPendingFrame
    );
    assert_eq!(container.scroll_height(), height_before);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(row_at_reader(&container), reader_before);
    assert_eq!(
        sb_rows(&sb_el(&container))[..history_nodes.len()],
        history_nodes[..]
    );

    assert_eq!(
        renderer.prepare_live_interaction(),
        RECONCILED_WITH_NEW_ANCHOR
    );
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.sb_base, 760);
    assert_eq!(
        frame
            .scrollback_rows
            .iter()
            .map(|entry| entry.index)
            .collect::<Vec<_>>(),
        [760]
    );
    assert_eq!(spans_text(&frame.viewport_rows[0].spans), "latest-v");
    let anchor = renderer.backfill_anchor().unwrap();
    assert_eq!((anchor.sb_base, anchor.total), (500, 761));
    assert_eq!(
        sb_rows(&sb_el(&container))[..history_nodes.len()],
        history_nodes[..]
    );
    assert_eq!(renderer.paint_presentation(None).tail_gap_px, 10.0 * ROW_PX);
    assert_eq!(container.scroll_top_writes(), 1);
    assert!(renderer.at_bottom());
    assert_eq!(renderer.reconciled_epoch_seq(), epoch_seq("test-grid:0", 4));
    assert_eq!(
        renderer.reconcile_block_reason(),
        ReconcileBlockReason::None
    );
}

#[test]
fn a_viewport_only_renewal_retains_equivalent_painted_rows_without_a_bridge() {
    let (container, mut renderer) = seeded("old-v");
    let existing = sb_rows(&sb_el(&container));
    let renewal = CellGridFrame {
        stream_id: "test-stream:2".to_string(),
        seq: 2,
        ..full_frame(80, vec![row(0, "new-v")], 752)
    };
    assert!(renderer.apply_full_frame(&renewal));
    let frame = renderer.current_frame().unwrap();
    assert!(frame.scrollback_rows.is_empty());
    assert_eq!(frame.sb_base, 752);
    assert_eq!(sb_rows(&sb_el(&container))[..existing.len()], existing[..]);
}

#[test]
fn an_incompatible_full_keeps_the_reader_window_immutable_until_explicit_resume() {
    let (container, mut renderer) = seeded("old-v");
    let old_nodes = sb_rows(&sb_el(&container));
    at_reader_row(&container, &mut renderer, 600.0);
    let parked = Some(ReaderAnchor {
        row: 600,
        offset_px: 0.0,
    });
    assert_eq!(renderer.paint_presentation(None).reader_anchor, parked);
    container.reset_scroll_top_writes();

    let next_epoch = CellGridFrame {
        stream_id: "test-stream:1".to_string(),
        grid_epoch: "test-grid:1".to_string(),
        ..full_frame(100, vec![row(0, "new-epoch-v")], 760)
    };
    assert!(renderer.apply_full_frame(&next_epoch));
    assert_eq!(renderer.backfill_anchor(), None);
    assert_eq!(renderer.paint_presentation(None).reader_anchor, parked);
    assert_eq!(renderer.current_frame().unwrap().grid_epoch, "test-grid:0");
    assert_eq!(renderer.canonical_epoch_seq(), epoch_seq("test-grid:1", 1));
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(
        sb_rows(&sb_el(&container))[..old_nodes.len()],
        old_nodes[..]
    );

    assert_eq!(
        renderer.prepare_live_interaction(),
        RECONCILED_WITH_NEW_ANCHOR
    );
    assert_eq!(renderer.current_frame().unwrap().grid_epoch, "test-grid:1");
    assert_eq!(
        renderer.reconciled_epoch_seq(),
        renderer.canonical_epoch_seq()
    );
    let painted = sb_rows(&sb_el(&container));
    assert!(old_nodes.iter().all(|node| !painted.contains(node)));
    assert_eq!(
        container.scroll_top(),
        container.scroll_height() - container.client_height()
    );
}

#[test]
fn releasing_selection_reconciles_its_pending_canonical_state() {
    let (container, mut renderer) = seeded("old-v");
    container.set_scroll_top_raw(PAD_TOP + 600.0 * ROW_PX);
    renderer.set_selection_hold(true);
    renderer.apply(&CellGridFrame {
        seq: 3,
        ..full_frame(80, vec![row(0, "latest-v")], 760)
    });
    container.reset_scroll_top_writes();
    assert_eq!(
        renderer.set_selection_hold(false),
        RECONCILED_WITH_NEW_ANCHOR
    );
    let frame = renderer.current_frame().unwrap();
    assert_eq!(frame.seq, 3);
    assert_eq!(
        renderer.reader_intent(),
        roost_web_terminal::ReaderIntent::Live
    );
    assert_eq!(spans_text(&frame.viewport_rows[0].spans), "latest-v");
    assert_eq!(
        container.scroll_top(),
        container.scroll_height() - container.client_height()
    );
    assert_eq!(container.scroll_top_writes(), 1);
}
