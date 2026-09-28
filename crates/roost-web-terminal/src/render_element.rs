//! The element seam the renderer paints through: every DOM read and write
//! `CellGridRenderer` performs, as one trait over an element handle.
//!
//! Production is `web_sys::Element` (`render_element/web.rs`, the one
//! `wasm32` adapter); the native test tier supplies an in-memory element with
//! the layout model v2's `apps/web/tests/helpers/cellRendererFakeDom.ts` used,
//! so the renderer's paint, reader and history rules run without a browser.

#[cfg(target_arch = "wasm32")]
mod web;

use std::fmt;

use crate::cell_renderer_dom::DomResult;

/// One element's client-space box, in CSS pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ElementRect {
    /// Distance from the viewport's left edge.
    pub left: f64,
    /// Distance from the viewport's top edge.
    pub top: f64,
    /// The box's width.
    pub width: f64,
    /// The box's height.
    pub height: f64,
}

/// An element handle the renderer can build, arrange, stamp and measure.
///
/// Cloning a handle clones the REFERENCE, never the node, and `PartialEq` is
/// node identity: two handles are equal exactly when they name the same node.
/// Every write is total — a node the document refuses to change is left as it
/// was, and the reconcile watermark repairs on the next frame — so no method
/// here returns an error except element creation.
pub trait RenderElement: Clone + PartialEq + fmt::Debug {
    /// Create a detached element of `tag` in this element's document.
    fn create_element(&self, tag: &str) -> DomResult<Self>;
    /// Append `child` as this element's last child, moving it if it is placed.
    fn append_child(&self, child: &Self);
    /// Insert `child` before `reference`, or append it when there is none.
    fn insert_before(&self, child: &Self, reference: Option<&Self>);
    /// Detach this element from its parent; a detached element is a no-op.
    fn remove(&self);
    /// Put `replacement` where this element sits among its siblings.
    fn replace_with(&self, replacement: &Self);
    /// Remove every child.
    fn clear_children(&self);
    /// The parent element, if the element is placed.
    fn parent(&self) -> Option<Self>;
    /// How many element children this element has.
    fn child_count(&self) -> u32;
    /// One element child, by position.
    fn child_at(&self, index: u32) -> Option<Self>;
    /// The first element child.
    fn first_child(&self) -> Option<Self> {
        self.child_at(0)
    }
    /// The whole `class` attribute.
    fn class_name(&self) -> String;
    /// Replace the whole `class` attribute.
    fn set_class_name(&self, name: &str);
    /// Add one class to the class list.
    fn add_class(&self, name: &str);
    /// Force one class on or off.
    fn toggle_class(&self, name: &str, on: bool);
    /// One attribute's value.
    fn attribute(&self, name: &str) -> Option<String>;
    /// Set one attribute.
    fn set_attribute(&self, name: &str, value: &str);
    /// Drop one attribute.
    fn remove_attribute(&self, name: &str);
    /// Replace the element's content with one text node.
    fn set_text(&self, text: &str);
    /// Set one inline CSS property.
    fn set_style(&self, property: &str, value: &str);
    /// Drop one inline CSS property.
    fn remove_style(&self, property: &str);
    /// The scroll position, as the double the DOM reports.
    fn scroll_top(&self) -> f64;
    /// Write the scroll position.
    fn set_scroll_top(&self, value: f64);
    /// The scrollable content height.
    fn scroll_height(&self) -> f64;
    /// The box's inner height.
    fn client_height(&self) -> f64;
    /// The element's offset from its offset parent's top.
    fn offset_top(&self) -> f64;
    /// The element's client-space box.
    fn bounding_rect(&self) -> ElementRect;
    /// Whether the element is in a document.
    fn is_connected(&self) -> bool;
    /// The document's monotonic clock, in milliseconds since its time origin;
    /// zero where the document has no clock.
    fn now_ms(&self) -> f64;
}
