//! The production `FindHost`: the pane's `CellGridRenderer` answering the find
//! controller over its public methods — the backfill anchor, the highlight
//! publication, the find-owned scroll and the end of the find reading. The
//! terminal pane passes its renderer to every `TerminalFind` call. Ports the
//! renderer calls of `apps/web/src/renderer/terminalFindController.ts`.

use crate::cell_renderer::CellGridRenderer;
use crate::find::hits::{ActiveHit, HitRows};
use crate::find::host::FindHost;
use crate::presentation::BackfillAnchor;
use crate::render_element::RenderElement;

impl<E: RenderElement> FindHost for CellGridRenderer<E> {
    fn anchor(&self) -> Option<BackfillAnchor> {
        self.backfill_anchor()
    }

    fn publish_hits(&mut self, hits: HitRows, active: Option<ActiveHit>) {
        self.set_find_highlights(hits.into_rows(), active.map(|hit| (hit.row, hit.col)));
    }

    fn reveal_row(&mut self, row: u32) {
        if !self.scroll_to_scrollback_row(row) {
            tracing::debug!(target: "find", row, "find reveal found its row no longer painted");
        }
    }

    fn end_find_read(&mut self) {
        self.end_find_reading();
    }
}
