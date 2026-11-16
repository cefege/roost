//! DOM measurement and style publication for an open dropdown submenu.
//! The submenu panel calls this after mounting; pure placement math stays in
//! `ui::placement` and the parent owns menu lifecycle and focus.

use dioxus::prelude::*;
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use super::{DropdownMenuSubSide, SubmenuRoot};
#[cfg(target_arch = "wasm32")]
use crate::components::ui::dom::client_rect_of;
#[cfg(target_arch = "wasm32")]
use crate::components::ui::placement::place_submenu;

pub(super) async fn place_panel(
    context: Signal<SubmenuRoot>,
    mounted: Rc<MountedData>,
    style: Signal<String>,
    side: DropdownMenuSubSide,
) {
    let Ok(panel_rect) = mounted.get_client_rect().await else {
        return;
    };
    #[cfg(target_arch = "wasm32")]
    {
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(trigger) = window
            .document()
            .and_then(|document| document.get_element_by_id(&context.read().trigger_id))
            .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
        else {
            return;
        };
        let placement = place_submenu(
            client_rect_of(&trigger),
            panel_rect.size.width,
            panel_rect.size.height,
            window
                .inner_width()
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0),
            window
                .inner_height()
                .ok()
                .and_then(|value| value.as_f64())
                .unwrap_or(0.0),
            side,
        );
        let mut style = style;
        style.set(placement.style());
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (context, panel_rect, style, side);
}
