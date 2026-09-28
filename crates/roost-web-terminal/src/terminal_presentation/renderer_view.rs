//! The production `PresentationRendererView`: the pane's own `CellGridRenderer`,
//! over whichever element seam it paints through. Carries the two reconcile
//! watermark reads, the reader park reason and the cursor-blink write that
//! `apps/web/src/renderer/terminalPresentation.ts` makes on the renderer.

use super::PresentationRendererView;
use crate::cell_renderer::CellGridRenderer;
use crate::presentation::RendererEpochSeq;
use crate::reader_intent::ReaderIntentReason;
use crate::render_element::RenderElement;

impl<E: RenderElement> PresentationRendererView for CellGridRenderer<E> {
    fn canonical_epoch_seq(&self) -> RendererEpochSeq {
        CellGridRenderer::canonical_epoch_seq(self)
    }

    fn reconciled_epoch_seq(&self) -> RendererEpochSeq {
        CellGridRenderer::reconciled_epoch_seq(self)
    }

    fn reader_reason(&self) -> Option<ReaderIntentReason> {
        CellGridRenderer::reader_reason(self)
    }

    fn set_cursor_blink_enabled(&mut self, enabled: bool) {
        CellGridRenderer::set_cursor_blink_enabled(self, enabled);
    }
}
