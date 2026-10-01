//! The two document touches the overlay surfaces need: focusing a control by
//! its test id after the frame that mounted it, and keeping a keyboard-highlighted
//! row in view. Called by `palette::body` and `help::overlay`; depends on
//! nothing but `web_sys`.
//!
//! Both wait for the next frame on purpose. A modal moves focus into its own
//! focus trap when it mounts, and that mount and the body's mount are the same
//! frame, so a focus issued during render is the one that loses.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast as _;
#[cfg(target_arch = "wasm32")]
use web_sys::{Element, ScrollIntoViewOptions, ScrollLogicalPosition};

/// Focus the element carrying `test_id` on the next animation frame.
///
/// A `data-testid` rather than an id: the test id is the contract the specs
/// select on, and an id invented here would be a second name for one control.
#[cfg(target_arch = "wasm32")]
pub fn focus_by_test_id_next_frame(test_id: &str) {
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let selector = format!("[data-testid=\"{test_id}\"]");
    let focus = Closure::once_into_js(move || {
        if let Some(element) = query(&selector)
            && let Ok(html) = element.dyn_into::<web_sys::HtmlElement>()
        {
            let _ = html.focus();
        }
    });
    let _ = window.request_animation_frame(focus.unchecked_ref());
}

/// A native build has no document and no frame.
#[cfg(not(target_arch = "wasm32"))]
pub fn focus_by_test_id_next_frame(_test_id: &str) {}

/// Scroll the `index`th element carrying `test_id` into view.
///
/// `block: "nearest"` is the whole point: a full-alignment scroll would move the
/// page every time the highlight changed, including when the pointer — not the
/// keyboard — moved it.
#[cfg(target_arch = "wasm32")]
pub fn scroll_row_into_view(test_id: &str, index: usize) {
    let selector = format!("[data-testid=\"{test_id}\"]");
    let Some(element) = nth(&selector, index) else {
        return;
    };
    let options = ScrollIntoViewOptions::new();
    options.set_block(ScrollLogicalPosition::Nearest);
    element.scroll_into_view_with_scroll_into_view_options(&options);
}

/// Nothing scrolls without a document.
#[cfg(not(target_arch = "wasm32"))]
pub fn scroll_row_into_view(_test_id: &str, _index: usize) {}

#[cfg(target_arch = "wasm32")]
fn query(selector: &str) -> Option<Element> {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector(selector).ok().flatten())
}

#[cfg(target_arch = "wasm32")]
fn nth(selector: &str, index: usize) -> Option<Element> {
    web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector_all(selector).ok())
        .and_then(|nodes| nodes.item(u32::try_from(index).unwrap_or_default()))
        .and_then(|node| node.dyn_into::<Element>().ok())
}
