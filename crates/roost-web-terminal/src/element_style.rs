//! The DOM properties the wasm32 adapters need that `web_sys::Element` does not
//! expose: inline `style`, which lives on `HTMLElement`, and the double-valued
//! `scrollTop`. Read by the renderer, link, reader-scroll, echo and mouse
//! adapters; every call is total. Ports the `style` and `scrollTop` access
//! of `apps/web/src/renderer/cellRenderer.ts` and `terminal-links.scan.ts`.

use js_sys::Reflect;
use wasm_bindgen::{JsCast, JsValue};
use web_sys::Element;

/// Set one inline CSS property, or do nothing when the element is not an HTML
/// element and has no inline style declaration to set it on.
pub fn set_style_property(element: &Element, property: &str, value: &str) {
    if let Some(html) = element.dyn_ref::<web_sys::HtmlElement>() {
        let _ = html.style().set_property(property, value);
    }
}

/// Drop one inline CSS property, or do nothing when the element is not an HTML
/// element. A cleared property is how a sealed block rejoins the browser's
/// skipped-subtree path, so it has to be as unconditional as setting one.
pub fn remove_style_property(element: &Element, property: &str) {
    if let Some(html) = element.dyn_ref::<web_sys::HtmlElement>() {
        let _ = html.style().remove_property(property);
    }
}

/// One inline CSS property, verbatim; empty when it is unset or the element is
/// not an HTML element, which is what `style.getPropertyValue` answers.
pub fn style_property_of(element: &Element, property: &str) -> String {
    element
        .dyn_ref::<web_sys::HtmlElement>()
        .and_then(|html| html.style().get_property_value(property).ok())
        .unwrap_or_default()
}

/// The element's scroll position in CSS pixels, as the DOUBLE the DOM reports.
///
/// `Element::scroll_top` is an `i32` on the stable surface. Rounding to whole
/// pixels makes a reader that moved half a row read as unmoved, and "did the
/// reader move" is exactly what the follow-band predicate and the owned-write
/// check are asked, so the fraction is read off the property itself. An element
/// that reports no position reads as zero, which is the top of its scroll range
/// and therefore inert for every predicate that compares it against a maximum.
pub fn scroll_top_of(element: &Element) -> f64 {
    let target: &JsValue = element.as_ref();
    Reflect::get(target, &JsValue::from_str("scrollTop"))
        .ok()
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0)
}

/// Write the element's scroll position in CSS pixels.
///
/// A write the document refuses leaves the position where it was, and the
/// caller re-reads it to decide whether the write happened at all, so the
/// refusal needs no error path of its own.
pub fn set_scroll_top_of(element: &Element, value: f64) {
    let target: &JsValue = element.as_ref();
    let _ = Reflect::set(
        target,
        &JsValue::from_str("scrollTop"),
        &JsValue::from_f64(value),
    );
}
