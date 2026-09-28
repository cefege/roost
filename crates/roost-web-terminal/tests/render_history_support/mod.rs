//! History fixtures shared by the truthful-scroll-space test binaries: a
//! seeded 250-row painted history, the reader row by geometry, epoch/seq and
//! range builders. A test binary pulls it in with `mod render_support;` then
//! `mod render_history_support;`. Ported from the helpers of
//! `apps/web/tests/renderer/cellRenderer.history.dom.test.ts`.

#![allow(dead_code)]

use roost_client_core::terminal::history::HistoryRange;
use roost_protocol::cell::{CellGridFrame, CellRow};
use roost_web_terminal::RenderElement;
use roost_web_terminal::presentation::{LiveInteractionResult, RendererEpochSeq};

use crate::render_support::{
    FakeEl, FakeRenderer, PAD_TOP, ROW_PX, delta_frame, mount, numbered_rows, row, sb_el, sb_rows,
    seed_held_history_to,
};

pub fn append_delta(append: Vec<CellRow>, total: u64, seq: u64) -> CellGridFrame {
    CellGridFrame {
        scrollback_total: total,
        ..delta_frame(80, 1, vec![row(0, "v")], append, seq)
    }
}

/// The painted row the reader's pixel offset lands on, by GEOMETRY.
pub fn row_at_reader(container: &FakeEl) -> Option<String> {
    let scrollback = sb_el(container);
    let index = ((container.scroll_top() - scrollback.offset_top()) / ROW_PX).floor() as usize;
    sb_rows(&scrollback).get(index).map(FakeEl::text_content)
}

pub fn range(start: u32, end: u32) -> HistoryRange {
    HistoryRange { start, end }
}

pub fn epoch_seq(grid_epoch: &str, seq: u64) -> RendererEpochSeq {
    RendererEpochSeq {
        grid_epoch: Some(grid_epoch.to_string()),
        seq: Some(seq),
    }
}

pub const RECONCILED_WITH_NEW_ANCHOR: LiveInteractionResult = LiveInteractionResult {
    reconciled: true,
    anchor_changed: true,
};

/// 250 painted rows `[500, 750)` under a single viewport row.
pub fn seeded(viewport: &str) -> (FakeEl, FakeRenderer) {
    let (container, mut renderer) = mount();
    assert!(seed_held_history_to(
        &mut renderer,
        80,
        vec![row(0, viewport)],
        numbered_rows(250, 500),
        750
    ));
    (container, renderer)
}

pub fn at_reader_row(container: &FakeEl, renderer: &mut FakeRenderer, absolute_row: f64) {
    container.set_scroll_top_raw(PAD_TOP + absolute_row * ROW_PX);
    renderer.handle_scroll();
}
