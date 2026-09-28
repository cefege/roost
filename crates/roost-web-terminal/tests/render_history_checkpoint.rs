//! A viewport-only checkpoint cannot prove which rows left the grid, so the
//! rows it transitioned stay an UNPAINTED gap that owns its pixels until the
//! worker's own page fills it — never a stale repaint generation inferred from
//! the old viewport. Ported from the checkpoint cases of
//! `apps/web/tests/renderer/cellRenderer.history.dom.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_support;

use render_support::{
    PAD_TOP, ROW_PX, delta_frame, full_frame, mount, numbered_rows, row, sb_el, sb_rows,
    seed_held_history_to,
};
use roost_client_core::terminal::history::HistoryRange;
use roost_protocol::cell::CellGridFrame;
use roost_web_terminal::presentation::PaintedRowText;
use roost_web_terminal::{MAX_HELD_SCROLLBACK_ROWS, RenderElement};

fn painted(entries: &[(u32, &str)]) -> Option<Vec<PaintedRowText>> {
    Some(
        entries
            .iter()
            .map(|(index, text)| PaintedRowText {
                index: *index,
                text: text.to_string(),
            })
            .collect(),
    )
}

#[test]
fn a_checkpoint_leaves_the_transitioned_rows_unpainted_for_authoritative_backfill() {
    let (container, mut renderer) = mount();
    let old_viewport = vec![row(0, "old-0"), row(1, "old-1"), row(2, "old-2")];
    assert!(renderer.apply_full_frame(&CellGridFrame {
        seq: 1,
        ..full_frame(80, old_viewport, 10)
    }));
    let new_viewport = vec![row(0, "new-0"), row(1, "new-1"), row(2, "new-2")];
    assert!(renderer.apply_full_frame(&CellGridFrame {
        seq: 2,
        ..full_frame(80, new_viewport, 12)
    }));

    assert!(renderer.current_frame().unwrap().scrollback_rows.is_empty());
    assert!(!renderer.has_painted_scrollback_range(10, 12));
    assert_eq!(renderer.painted_scrollback_range(10, 12), None);
    assert!(sb_rows(&sb_el(&container)).is_empty());
    assert_eq!(
        renderer.missing_scrollback_range(10),
        Some(HistoryRange { start: 0, end: 12 })
    );
    assert_eq!(renderer.paint_presentation(None).tail_gap_px, 2.0 * ROW_PX);
    assert_eq!(container.scroll_height(), PAD_TOP + (12.0 + 3.0) * ROW_PX);

    assert!(renderer.insert_history_page(&[row(10, "worker-10"), row(11, "worker-11")], false));
    assert_eq!(
        renderer.painted_scrollback_range(10, 12),
        painted(&[(10, "worker-10"), (11, "worker-11")])
    );
    assert_eq!(renderer.paint_presentation(None).tail_gap_px, 0.0);
    assert_eq!(container.scroll_height(), PAD_TOP + (12.0 + 3.0) * ROW_PX);
}

#[test]
fn a_checkpoint_never_paints_a_stale_repaint_generation_into_history() {
    let (_container, mut renderer) = mount();
    let block = vec![
        row(0, "header 7m"),
        row(1, "spinner 7m"),
        row(2, "log-c"),
        row(3, "log-d"),
    ];
    assert!(seed_held_history_to(
        &mut renderer,
        80,
        block,
        numbered_rows(8, 0),
        8
    ));
    // Cursor-up and rewrite: the TUI repaints its card block in place.
    assert!(renderer.apply(&CellGridFrame {
        scrollback_total: 8,
        ..delta_frame(
            80,
            4,
            vec![row(0, "header 5m"), row(1, "spinner 5m")],
            Vec::new(),
            3
        )
    }));
    // The worker repainted again and only then scrolled two rows, so the rows
    // that really left the viewport carry the LATER generation.
    let scrolled = vec![
        row(0, "log-c"),
        row(1, "log-d"),
        row(2, "log-e"),
        row(3, "log-f"),
    ];
    assert!(renderer.apply_full_frame(&CellGridFrame {
        seq: 4,
        ..full_frame(80, scrolled, 10)
    }));

    let texts: Vec<String> = renderer
        .paint_presentation(None)
        .rows
        .into_iter()
        .map(|entry| entry.text)
        .collect();
    assert!(
        !texts
            .iter()
            .any(|text| text == "header 5m" || text == "spinner 5m"),
        "{texts:?}"
    );
    assert!(!renderer.has_painted_scrollback_range(8, 10));
    assert_eq!(
        renderer.missing_scrollback_range(9),
        Some(HistoryRange { start: 8, end: 10 })
    );
    assert_eq!(renderer.paint_presentation(None).tail_gap_px, 2.0 * ROW_PX);

    assert!(renderer.insert_history_page(&[row(8, "header 3m"), row(9, "spinner 3m")], false));
    assert_eq!(
        renderer.painted_scrollback_range(8, 10),
        painted(&[(8, "header 3m"), (9, "spinner 3m")])
    );
    assert!(renderer.has_painted_scrollback_range(0, 10));
    assert_eq!(renderer.painted_scrollback_row_count(), 10);
}

#[test]
fn a_checkpoint_keeps_demanded_head_coverage_and_reserves_the_tail_gap() {
    let (_container, mut renderer) = mount();
    let held = MAX_HELD_SCROLLBACK_ROWS as u32;
    assert!(seed_held_history_to(
        &mut renderer,
        80,
        vec![row(0, "old-v")],
        numbered_rows(held, 0),
        u64::from(held)
    ));
    let renewal = CellGridFrame {
        seq: 3,
        ..full_frame(80, vec![row(0, "new-v")], u64::from(held) + 1)
    };
    assert!(renderer.apply_full_frame(&renewal));

    assert!(renderer.has_painted_scrollback_range(0, held));
    assert_eq!(
        renderer
            .paint_presentation(Some(MAX_HELD_SCROLLBACK_ROWS + 1))
            .rows
            .len(),
        MAX_HELD_SCROLLBACK_ROWS
    );
    assert_eq!(
        renderer.painted_scrollback_range(0, 1),
        painted(&[(0, "r0")])
    );
    assert!(!renderer.has_painted_scrollback_range(held, held + 1));
    assert_eq!(renderer.paint_presentation(None).tail_gap_px, ROW_PX);
}
