//! What the pane registry may read off one mounted pane's renderer: viewport
//! and history text for dictation context, and preview rows for the tab grid.
//! `CellGridRenderer` behind its `RefCell` is the production surface; a borrow
//! that is busy (a paint in progress) answers `None` rather than panicking.

use std::cell::RefCell;

use roost_protocol::cell::{CellGridFrame, CellRow, spans_text};
use roost_web_terminal::CellGridRenderer;
use roost_web_terminal::render_element::RenderElement;

/// The read side of one mounted pane.
pub trait PaneSurface {
    /// The canonical viewport as text, one line per row.
    fn viewport_text(&self) -> Option<String>;
    /// The newest `max_rows` history rows of the applied frame as text.
    fn scrollback_text(&self, max_rows: usize) -> Option<String>;
    /// The newest non-blank rows of the current frame, oldest first.
    fn preview_rows(&self) -> Option<Vec<CellRow>>;
}

/// Rows a tab-grid preview shows: at most this many non-blank rows.
pub const MAX_PREVIEW_ROWS: usize = 18;
/// History rows scanned beside the viewport for a preview.
pub const PREVIEW_CANDIDATE_SCAN: usize = 20;

/// The newest non-blank rows of a frame's history tail and viewport, oldest
/// first. An alt-screen frame has no history, so its viewport alone is the
/// content.
pub fn preview_rows_of(frame: &CellGridFrame) -> Vec<CellRow> {
    let tail = frame
        .scrollback_rows
        .len()
        .saturating_sub(PREVIEW_CANDIDATE_SCAN);
    let mut picked: Vec<CellRow> = frame.scrollback_rows[tail..]
        .iter()
        .chain(frame.viewport_rows.iter())
        .rev()
        .filter(|row| !spans_text(&row.spans).trim().is_empty())
        .take(MAX_PREVIEW_ROWS)
        .cloned()
        .collect();
    picked.reverse();
    picked
}

impl<E: RenderElement> PaneSurface for RefCell<CellGridRenderer<E>> {
    fn viewport_text(&self) -> Option<String> {
        Some(self.try_borrow().ok()?.grid_text())
    }

    fn scrollback_text(&self, max_rows: usize) -> Option<String> {
        Some(self.try_borrow().ok()?.scrollback_text(max_rows))
    }

    fn preview_rows(&self) -> Option<Vec<CellRow>> {
        let renderer = self.try_borrow().ok()?;
        let frame = renderer.current_frame()?;
        (!frame.viewport_rows.is_empty()).then(|| preview_rows_of(frame))
    }
}
