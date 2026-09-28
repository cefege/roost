//! The two DOM properties the renderer needs that `web_sys::Element` does not
//! expose, on one named seam.
//!
//! `style` lives on `HTMLElement` and the double-valued `scrollTop` sits behind
//! `web_sys_unstable_apis`, so the stable `Element` surface offers neither. The
//! renderer holds every painted node as an `Element` — the structural `Node`
//! calls are unambiguous that way, and a web-sys element implements `AsRef` for
//! its whole IDL chain — so these two functions are where that gap is closed,
//! once, instead of at every call site.
//!
//! Both are total: a node that is not an HTML element, or a property the
//! document refuses, is a no-op. The renderer's elements are `div`, `span` and
//! `a`, so the cast succeeds; a caller that hands it something else gets an
//! unpainted element rather than a panic, and the reconcile watermark repaints.

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
