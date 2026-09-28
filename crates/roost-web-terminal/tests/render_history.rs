//! Truthful scroll space: the `[0, sb_base)` head spacer and in-place gap pages
//! keep every absolute row at a stable pixel offset while history materializes.
//! Ported from `apps/web/tests/renderer/cellRenderer.history.dom.test.ts` (the
//! live repair and renewal half lives in `render_history_repair.rs`, the
//! checkpoint half in `render_history_checkpoint.rs`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod render_history_support;
mod render_support;

use render_history_support::{append_delta, range, row_at_reader, seeded};
use render_support::{
    PAD_TOP, ROW_PX, full_frame, mount, numbered_rows, row, sb_el, sb_rows, seed_held_history_to,
    spacer_el,
};
use roost_protocol::cell::CellGridFrame;
use roost_web_terminal::{MAX_HELD_SCROLLBACK_ROWS, RenderElement};

#[test]
fn the_spacer_reserves_the_unpainted_history() {
    let (container, renderer) = seeded("v");
    assert_eq!(
        spacer_el(&container).style("height").as_deref(),
        Some("8000.00px")
    );
    assert_eq!(container.scroll_height(), PAD_TOP + (750.0 + 1.0) * ROW_PX);
    container.set_scroll_top_raw(PAD_TOP + 100.0 * ROW_PX);
    let visible = renderer.missing_scrollback_range_at_scroll(0).unwrap();
    assert_eq!(
        (visible.missing, visible.focus_row, visible.in_window.end),
        (range(0, 500), 100, 132)
    );
    container.set_scroll_top_raw(PAD_TOP + 600.0 * ROW_PX);
    assert_eq!(renderer.missing_scrollback_range_at_scroll(0), None);
    let ahead = renderer.missing_scrollback_range_at_scroll(500).unwrap();
    assert_eq!(
        (ahead.missing, ahead.focus_row, ahead.in_window.end),
        (range(0, 500), 100, 500)
    );
}

#[test]
fn a_head_page_shrinks_the_spacer_by_exactly_the_rows_it_adds() {
    let (container, mut renderer) = seeded("v");
    container.set_scroll_top_raw(PAD_TOP + 600.0 * ROW_PX);
    let height_before = container.scroll_height();
    let reader_before = row_at_reader(&container);
    container.reset_scroll_top_writes();
    assert!(renderer.insert_history_page(&numbered_rows(250, 250), false));
    assert_eq!(
        spacer_el(&container).style("height").as_deref(),
        Some("4000.00px")
    );
    assert_eq!(container.scroll_height(), height_before);
    assert_eq!(container.scroll_top_writes(), 0);
    assert_eq!(row_at_reader(&container), reader_before);
    assert_eq!(reader_before.as_deref(), Some("r600"));
}

#[test]
fn targeted_pages_split_a_tail_gap_without_replacing_existing_rows() {
    let (container, mut renderer) = seeded("v");
    assert!(renderer.apply_full_frame(&CellGridFrame {
        seq: 3,
        ..full_frame(80, vec![row(0, "v")], 760)
    }));
    let existing = sb_rows(&sb_el(&container));
    assert_eq!(
        renderer.missing_scrollback_range(755),
        Some(range(750, 760))
    );
    assert!(renderer.insert_history_page(&numbered_rows(6, 751), false));
    assert!(!renderer.insert_history_page(&[row(756, "duplicate"), row(757, "new")], false));
    assert_eq!(
        renderer.missing_scrollback_range(759),
        Some(range(757, 760))
    );
    assert_eq!(renderer.paint_presentation(None).tail_gap_px, 4.0 * ROW_PX);
    assert_eq!(sb_rows(&sb_el(&container))[..existing.len()], existing[..]);
    assert!(renderer.insert_history_page(&numbered_rows(3, 757), false));
    assert_eq!(renderer.missing_scrollback_range(759), None);
    assert_eq!(
        renderer.missing_scrollback_range(750),
        Some(range(750, 751))
    );
    assert!(renderer.insert_history_page(&numbered_rows(1, 750), false));
    assert!(renderer.has_painted_scrollback_range(750, 760));
    assert_eq!(sb_rows(&sb_el(&container)).len(), 260);
}

#[test]
fn an_interior_page_splits_only_its_placeholder() {
    let (container, mut renderer) = seeded("v");
    let existing = sb_rows(&sb_el(&container));
    assert!(renderer.insert_history_page(&numbered_rows(50, 250), false));
    assert_eq!(
        renderer.missing_scrollback_range(375),
        Some(range(300, 500))
    );
    assert!(renderer.insert_history_page(&numbered_rows(50, 350), false));
    assert_eq!(
        renderer.missing_scrollback_range(325),
        Some(range(300, 350))
    );
    assert_eq!(
        renderer.missing_scrollback_range(450),
        Some(range(400, 500))
    );
    let after = sb_rows(&sb_el(&container));
    assert_eq!(after[after.len() - existing.len()..], existing[..]);
}

#[test]
fn an_eviction_grows_the_spacer_by_exactly_the_rows_it_drops() {
    let (container, mut renderer) = mount();
    let held = 1900;
    assert!(seed_held_history_to(
        &mut renderer,
        80,
        vec![row(0, "v")],
        numbered_rows(held, 500),
        u64::from(500 + held)
    ));
    container.set_scroll_top_raw((container.scroll_height() - container.client_height()).max(0.0));
    let height_before = container.scroll_height();
    let spacer_before = spacer_el(&container).style_px("height");
    renderer.apply(&append_delta(
        numbered_rows(250, 500 + held),
        u64::from(750 + held),
        3,
    ));
    let frame = renderer.current_frame().unwrap();
    let dropped = frame.sb_base - 500;
    assert_eq!(dropped, 150);
    assert_eq!(frame.scrollback_rows.len(), MAX_HELD_SCROLLBACK_ROWS);
    assert_eq!(
        spacer_el(&container).style_px("height"),
        spacer_before + dropped as f64 * ROW_PX
    );
    assert_eq!(container.scroll_height(), height_before + 250.0 * ROW_PX);
}
