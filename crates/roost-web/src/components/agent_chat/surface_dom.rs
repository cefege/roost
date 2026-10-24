//! Browser-only transcript scrolling and delegated code-block copying.
//! Called by `surface`; uses the existing clipboard hook and mounted element.
//! Scroll ownership stays with the surface's pre-update reader decision.

use dioxus::prelude::*;

#[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
pub(super) fn copy_code_block(event: &MouseEvent) {
    #[cfg(target_arch = "wasm32")]
    {
        use dioxus::web::WebEventExt as _;
        use wasm_bindgen::JsCast as _;

        let Some(button) = event
            .as_web_event()
            .target()
            .and_then(|target| target.dyn_into::<web_sys::Element>().ok())
            .and_then(|element| {
                element
                    .closest(&format!("[{}]", super::markdown::COPY_CODE_ATTRIBUTE))
                    .ok()
                    .flatten()
            })
        else {
            return;
        };
        let code = button
            .closest("[data-code-block]")
            .ok()
            .flatten()
            .and_then(|block| block.query_selector("code").ok().flatten())
            .and_then(|code| code.text_content())
            .unwrap_or_default();
        crate::components::hooks::use_copy_clipboard::copy_text_then(&code, move |accepted| {
            if !accepted {
                return;
            }
            button.set_text_content(Some("Copied"));
            wasm_bindgen_futures::spawn_local(async move {
                crate::components::terminal::dom::sleep_ms(1_500).await;
                button.set_text_content(Some("Copy"));
            });
        });
    }
}

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
