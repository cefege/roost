//! The DOM reads the input-navigation adapters share: the focused element,
//! selector tests, scroll geometry and element rects. wasm32 only; every
//! decision these feed is made in the pure modules.
//! Called by `spatial_dom`, `pad_dom` and `keypad_focus`.

use roost_web_terminal::element_style::scroll_top_of;
use roost_web_terminal::reader_intent::ScrollBoxGeometry;
use wasm_bindgen::JsCast as _;
use web_sys::{Document, Element, HtmlElement};

use crate::input_nav::spatial::NavRect;

/// The window's document.
pub(crate) fn document() -> Option<Document> {
    web_sys::window().and_then(|window| window.document())
}

/// `document.activeElement`.
pub(crate) fn active_element() -> Option<Element> {
    document().and_then(|document| document.active_element())
}

/// Whether `element` is the document's `<body>`.
pub(crate) fn is_body(element: &Element) -> bool {
    document()
        .and_then(|document| document.body())
        .is_some_and(|body| body.unchecked_ref::<Element>() == element)
}

/// `element.matches(selector)`; a selector the browser rejects is no match.
pub(crate) fn matches(element: &Element, selector: &str) -> bool {
    element.matches(selector).unwrap_or(false)
}

/// `element.closest(selector)`.
pub(crate) fn closest(element: &Element, selector: &str) -> Option<Element> {
    element.closest(selector).ok().flatten()
}

/// The element as an `HTMLElement`, which is what can take focus.
pub(crate) fn html(element: &Element) -> Option<HtmlElement> {
    element.dyn_ref::<HtmlElement>().cloned()
}

/// The element's vertical scroll geometry, with the fractional `scrollTop`.
pub(crate) fn scroll_geometry(element: &Element) -> ScrollBoxGeometry {
    ScrollBoxGeometry {
        scroll_top: scroll_top_of(element),
        scroll_height: f64::from(element.scroll_height()),
        client_height: f64::from(element.client_height()),
    }
}

/// `getBoundingClientRect()`.
pub(crate) fn nav_rect(element: &Element) -> NavRect {
    let rect = element.get_bounding_client_rect();
    NavRect::new(rect.left(), rect.top(), rect.width(), rect.height())
}

/// `performance.now()`, or 0 where the page has no performance clock.
pub(crate) fn now_ms() -> f64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map_or(0.0, |performance| performance.now())
}
