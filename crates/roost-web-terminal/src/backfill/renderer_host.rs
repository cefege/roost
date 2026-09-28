//! The production `BackfillHost`: the pager reads and splices through the
//! renderer that owns the pane's DOM, exactly v2's `Pick<CellGridRenderer, …>`
//! in `apps/web/src/renderer/scrollbackBackfill.ts`. Pure delegation; every
//! rule lives in `CellGridRenderer` or the pager.

use roost_client_core::terminal::history::{HistoryRange, HistoryScrollTarget};
use roost_protocol::cell::CellRow;

use crate::backfill::BackfillHost;
use crate::cell_renderer::CellGridRenderer;
use crate::presentation::BackfillAnchor;
use crate::render_element::RenderElement;

impl<E: RenderElement> BackfillHost for CellGridRenderer<E> {
    fn backfill_anchor(&self) -> Option<BackfillAnchor> {
        CellGridRenderer::backfill_anchor(self)
    }
    fn follows_bottom(&self) -> bool {
        CellGridRenderer::follows_bottom(self)
    }
    fn has_painted_scrollback_range(&self, start: u32, end: u32) -> bool {
        CellGridRenderer::has_painted_scrollback_range(self, start, end)
    }
    fn missing_scrollback_range(&self, row: u32) -> Option<HistoryRange> {
        CellGridRenderer::missing_scrollback_range(self, row)
    }
    fn missing_scrollback_range_at_scroll(&self, ahead_rows: u32) -> Option<HistoryScrollTarget> {
        CellGridRenderer::missing_scrollback_range_at_scroll(self, ahead_rows)
    }
    fn set_history_floor(&mut self, row: u32) {
        CellGridRenderer::set_history_floor(self, row);
    }
    fn insert_history_page(&mut self, rows: &[CellRow], follow_tail: bool) -> bool {
        CellGridRenderer::insert_history_page(self, rows, follow_tail)
    }
}
