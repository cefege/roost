//! The explicit return target for product pointer menus.
//! Synthetic coordinate anchors must never replace the terminal or row opener.
//! Detached targets are skipped; Libero owns popover placement and dismissal.

#[cfg(target_arch = "wasm32")]
#[derive(Debug)]
pub struct FocusOpener(web_sys::HtmlElement);

#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
pub enum FocusOpener {}

#[cfg(target_arch = "wasm32")]
pub fn focused_element() -> Option<FocusOpener> {
    use wasm_bindgen::JsCast as _;
    web_sys::window()?
        .document()?
        .active_element()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()
        .map(FocusOpener)
}

#[cfg(not(target_arch = "wasm32"))]
pub fn focused_element() -> Option<FocusOpener> {
    None
}

#[cfg(target_arch = "wasm32")]
pub fn restore_focus(opener: &FocusOpener) {
    if opener.0.is_connected() {
        let options = web_sys::FocusOptions::new();
        options.set_prevent_scroll(true);
        let _ = opener.0.focus_with_options(&options);
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn restore_focus(opener: &FocusOpener) {
    match *opener {}
}
