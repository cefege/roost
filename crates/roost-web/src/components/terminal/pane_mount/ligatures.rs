//! Syncing the per-device shaping preference into the pane renderer.
//!
//! A changed choice rebuilds viewport and scrollback rows so every open pane
//! immediately uses its current text-run segmentation.

use super::PaneShared;
use roost_web_terminal::RenderElement;

pub(super) fn sync_ligatures(shared: &PaneShared, enabled: bool) {
    let value = if enabled { "true" } else { "false" };
    if shared.display.attribute("data-ligatures").as_deref() != Some(value) {
        let _ = shared.display.set_attribute("data-ligatures", value);
        if !shared.renderer.borrow_mut().refresh_cell_text_runs() {
            tracing::warn!(
                target: "terminal",
                session_id = %shared.session_id,
                "terminal rows could not be refreshed for the ligature preference"
            );
        }
    }
}
