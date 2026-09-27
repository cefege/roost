//! The DOM read and write behind a terminal link: painted rows in, link
//! attributes out. `links::scan` and `links::activation` hold every rule and are
//! tested without a browser; this file only moves data across the boundary.
//!
//! Browser-only by construction: the module is gated to `wasm32`, so a native
//! build of this crate never links a DOM type. Nothing here needs raw JS, so
//! nothing here needs `unsafe` — the one `dyn_ref` in the crate lives in
//! `element_style.rs` for properties `web_sys::Element` does not expose.

use wasm_bindgen::JsCast;
use web_sys::{Document, Element};

use super::{LinkActivation, PaintedChild, PaintedLinkAttributes, PaintedRow};
use crate::cell_row::{
    LINK_KEY_ATTR, ROW_HAS_LINKS_ATTR, TERMINAL_LINK_CLASS, TERMINAL_LINK_TARGET_ATTR,
};
use crate::element_style::set_style_property;
use crate::links::activation::{LinkModifierKey, link_title};

/// Read one painted row into the scanner's input.
///
/// A child's grid occupancy is measured from its own box rather than counted
/// from its text: a column is neither a character nor a code unit, and a CJK
/// ideograph is two columns in one. A child that cannot be measured claims no
/// columns, which keeps every later child's position from being invented.
pub fn read_painted_row(row: &Element, cell_width: f64) -> PaintedRow {
    let mut children = Vec::new();
    let mut node = row.first_child();
    while let Some(current) = node {
        node = current.next_sibling();
        let Some(element) = current.dyn_ref::<Element>() else {
            continue;
        };
        children.push(PaintedChild {
            columns: measured_columns(element, cell_width),
            link: read_link_attributes(element),
        });
    }
    PaintedRow {
        has_links: row.has_attribute(ROW_HAS_LINKS_ATTR),
        children,
    }
}

/// The link attributes one element carries, or `None` for a plain text run.
fn read_link_attributes(element: &Element) -> Option<PaintedLinkAttributes> {
    let attributes = PaintedLinkAttributes {
        is_terminal_link: element.class_list().contains(TERMINAL_LINK_CLASS),
        key: element.get_attribute(LINK_KEY_ATTR),
        target: element
            .get_attribute(TERMINAL_LINK_TARGET_ATTR)
            .or_else(|| element.get_attribute("href")),
    };
    attributes.is_terminal_link.then_some(attributes)
}

/// The grid columns an element covers, from its own measured width.
fn measured_columns(element: &Element, cell_width: f64) -> u32 {
    if cell_width <= 0.0 {
        return 0;
    }
    let Ok(rect) = element.get_bounding_client_rect() else {
        return 0;
    };
    let columns = ((rect.right() - rect.left()) / cell_width).round();
    if columns < 1.0 {
        0
    } else {
        columns as u32
    }
}

/// The terminal link anchor a pointer event landed on, or `None`.
///
/// `closest` walks up from the event's own target, so a click on a glyph nested
/// inside the anchor finds it. A selector the document refuses to parse is a
/// miss, not a failure: the constant is ours, so that would be a document that
/// cannot be trusted with a click.
pub fn anchor_under_point(target: &Element) -> Option<Element> {
    // The selector is built per call from another module's constant, so the two
    // cannot drift; a press is a user gesture, not a per-frame cost.
    target
        .closest(&format!("a.{TERMINAL_LINK_CLASS}"))
        .ok()
        .flatten()
}

/// Author the anchor's link attributes for the target it resolves to.
///
/// The `data-terminal-target` keeps the terminal-authored string even after a
/// file route is resolved, so a later click re-resolves against the same input
/// rather than against a route that may now be stale.
pub fn apply_link_attributes(
    anchor: &Element,
    raw_target: &str,
    activation: &LinkActivation,
    modifier_key: LinkModifierKey,
) {
    let _ = anchor.set_attribute(TERMINAL_LINK_TARGET_ATTR, raw_target);
    let _ = anchor.set_attribute("tabindex", "-1");
    let _ = anchor.set_attribute("draggable", "false");
    let _ = anchor.set_attribute("title", &link_title(modifier_key, activation.display()));
    match activation {
        LinkActivation::OpenExternal { href, display } => {
            let _ = anchor.set_attribute("href", href);
            let _ = anchor.set_attribute("target", "_blank");
            let _ = anchor.set_attribute("rel", "noopener noreferrer");
            let _ = anchor.remove_attribute("data-kind");
            let _ = anchor.set_attribute("data-hint", display);
        }
        LinkActivation::OpenWorkerFile { href, display } => {
            let _ = anchor.set_attribute("data-kind", "file");
            let _ = anchor.remove_attribute("target");
            let _ = anchor.remove_attribute("rel");
            let _ = anchor.set_attribute("href", href);
            let _ = anchor.set_attribute("data-hint", &format!("Open {display}"));
        }
    }
}

/// Open an external link the way a user click would: a real anchor, a real
/// click, and no lasting element.
///
/// The click is dispatched on a throwaway anchor rather than on the painted one
/// so the row's own attributes stay exactly what the renderer stamped.
pub fn open_external_link(
    doc: &Document,
    activation: &LinkActivation,
    modifier_key: LinkModifierKey,
) {
    let (Ok(anchor), Ok(body)) = (doc.create_element("a"), doc.body()) else {
        return;
    };
    apply_link_attributes(&anchor, activation.display(), activation, modifier_key);
    set_style_property(&anchor, "display", "none");
    let _ = body.append_child(&anchor);
    if let Ok(anchor) = anchor.dyn_ref::<web_sys::HtmlElement>() {
        anchor.click();
    }
    let _ = anchor.remove();
}
