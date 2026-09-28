//! The one reader-gesture scroll write for a terminal box, for input devices
//! that produce no trusted scroll event (a controller stick or D-pad). Ports
//! `apps/web/src/renderer/terminalReaderScroll.ts`; called by the pad-action
//! router. It writes and nothing else: the pane's own scroll listener classifies
//! and parks the reader exactly as it does for a wheel notch. The clamp is the
//! native `reader_scroll_target`; the DOM write is the wasm32 adapter.

use crate::reader_intent::ScrollBoxGeometry;

/// One controller scroll tick, in CSS pixels — roughly a wheel notch.
pub const PAD_SCROLL_STEP_PX: f64 = 96.0;

/// Where a box scrolled by `delta_px` lands, clamped to its scroll range, or
/// `None` when it cannot travel that way: it has no scroll range at all, or it
/// already rests at that edge.
///
/// `None` is the caller's signal to hand the direction to focus navigation
/// instead of dead-ending inside the pane, which is why an edge answers `None`
/// rather than the unchanged position.
pub fn reader_scroll_target(geometry: ScrollBoxGeometry, delta_px: f64) -> Option<f64> {
    let max = (geometry.scroll_height - geometry.client_height).max(0.0);
    if max <= 0.0 {
        return None;
    }
    let next = (geometry.scroll_top + delta_px).min(max).max(0.0);
    if next == geometry.scroll_top {
        return None;
    }
    Some(next)
}

/// Scroll `container` by `delta_px`, clamped, and answer whether it moved.
/// `false` means the box is already at that edge and nothing was written.
#[cfg(target_arch = "wasm32")]
pub fn scroll_terminal_reader_box(container: &web_sys::Element, delta_px: f64) -> bool {
    let geometry = ScrollBoxGeometry {
        scroll_top: crate::element_style::scroll_top_of(container),
        scroll_height: f64::from(container.scroll_height()),
        client_height: f64::from(container.client_height()),
    };
    let Some(next) = reader_scroll_target(geometry, delta_px) else {
        return false;
    };
    crate::element_style::set_scroll_top_of(container, next);
    tracing::debug!(target: "terminal", from = geometry.scroll_top, to = next,
        "reader box scrolled by a pad gesture");
    true
}
