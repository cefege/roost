//! The painted half of predictive local echo: one absolutely-positioned overlay
//! in a pane's viewport, one element per predicted cell. The predictor in
//! `roost-client-core` owns the burst and hands this crate an `EchoPaint`;
//! `geometry` turns that into the classes, offsets and inline CSS each cell is
//! stamped with, and the adapter below creates the elements. Creating them is
//! browser-only and gated to `wasm32`; the geometry is native and testable.

pub mod geometry;

pub use geometry::{
    OVERLAY_CLASS, PREDICTED_ERASE_CLASS, PREDICTED_GLYPH_CLASS, PaintedPrediction, cell_left,
    cell_top, plan_paint, prediction_style,
};

#[cfg(target_arch = "wasm32")]
use web_sys::Element;

#[cfg(target_arch = "wasm32")]
use crate::cell_renderer_dom::{DomResult, as_node, create_div, create_span, detach, is_child_of};
#[cfg(target_arch = "wasm32")]
use crate::element_style::set_style_property;

/// The one overlay element a pane paints its predictions into.
///
/// The predicted caret is NOT painted here. The pane hands `EchoPaint`'s
/// `caret_col` to `CellGridRenderer::set_predicted_cursor`, which is the one
/// writer of the painted caret and the only place the reconcile watermark
/// compares it against the column it intended to paint.
///
/// Owned separately from the grid rows so a prediction can never take part in
/// the row layout it covers, and so wiping a burst is one `set_inner_html`
/// rather than a walk over the grid.
#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct PredictiveEchoOverlay {
    overlay: Element,
    viewport: Element,
}

#[cfg(target_arch = "wasm32")]
impl PredictiveEchoOverlay {
    /// Create the overlay and attach it to the pane's viewport element.
    pub fn new(viewport: &Element) -> DomResult<Self> {
        let doc = viewport
            .owner_document()
            .ok_or_else(|| crate::cell_renderer_dom::DomSetupError::RefusedTag {
                tag: "document".to_string(),
            })?;
        let overlay = create_div(&doc)?;
        overlay.set_class_name(OVERLAY_CLASS);
        set_style_property(&overlay, "position", "absolute");
        set_style_property(&overlay, "top", "0");
        set_style_property(&overlay, "left", "0");
        // A prediction is decoration over the grid, never a target: a click that
        // landed on one would be a click on a terminal the user cannot see.
        set_style_property(&overlay, "pointer-events", "none");
        let _ = viewport.append_child(as_node(&overlay));
        Ok(Self {
            overlay,
            viewport: viewport.clone(),
        })
    }

    /// Repaint every predicted cell, or clear the overlay when `plan` is empty.
    pub fn paint(&self, plan: &[PaintedPrediction]) -> DomResult<()> {
        self.attach();
        let doc = self
            .overlay
            .owner_document()
            .ok_or_else(|| crate::cell_renderer_dom::DomSetupError::RefusedTag {
                tag: "document".to_string(),
            })?;
        self.overlay.set_inner_html("");
        for painted in plan {
            let element = create_span(&doc)?;
            element.set_class_name(painted.class_name);
            if !painted.text.is_empty() {
                element.set_text_content(Some(painted.text.as_str()));
            }
            let _ = element.set_attribute("style", &prediction_style(painted));
            let _ = self.overlay.append_child(as_node(&element));
        }
        Ok(())
    }

    /// Remove every painted prediction, leaving the element attached so the
    /// next paint costs no re-append.
    pub fn clear(&self) {
        self.overlay.set_inner_html("");
    }

    /// Detach the overlay with the pane.
    pub fn dispose(&self) {
        detach(&self.overlay);
    }

    /// The renderer rebuilds viewport children on a full repair, which detaches
    /// this overlay; re-append instead of silently painting into a dead node.
    fn attach(&self) {
        if !is_child_of(&self.overlay, &self.viewport) {
            let _ = self.viewport.append_child(as_node(&self.overlay));
        }
    }
}
