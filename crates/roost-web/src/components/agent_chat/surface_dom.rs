//! Browser-only transcript scrolling helpers.
//! Called by `surface`; scrolling follows the surface's reader decision.

#[cfg(target_arch = "wasm32")]
use dioxus::prelude::MountedData;
#[cfg(target_arch = "wasm32")]
fn transcript_element(node: &MountedData) -> Option<web_sys::HtmlElement> {
    use dioxus::web::WebEventExt as _;
    use wasm_bindgen::JsCast as _;
    node.try_as_web_event()
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
}

#[cfg(target_arch = "wasm32")]
pub(super) fn distance_from_bottom(node: &MountedData) -> Option<i32> {
    let element = transcript_element(node)?;
    Some(element.scroll_height() - element.scroll_top() - element.client_height())
}

#[cfg(target_arch = "wasm32")]
pub(super) fn scroll_to_bottom(node: &MountedData) {
    if let Some(element) = transcript_element(node) {
        element.set_scroll_top(element.scroll_height());
    }
}
