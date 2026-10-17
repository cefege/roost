//! DOM measurement for fixed floating panels anchored to a trigger found by element id.
//! Placement arithmetic lives in `components::menu_placement`; this file only reads the
//! trigger, its containing block and the viewport, and moves focus by element id.
//! Called by `ui::popover` and `ui::hover_card`; native builds return `None` and do nothing.

#[cfg(target_arch = "wasm32")]
use crate::components::deck::deck_dom::ClientBox;
#[cfg(target_arch = "wasm32")]
use crate::components::menu_placement::{place_menu, Horizontal, MenuPlacement, Vertical};

/// Inline `top`/`bottom`/`left`/`right` longhands that place a fixed panel beside the trigger
/// with id `trigger_id`, read from the live DOM. `None` when the trigger is not mounted.
#[cfg(target_arch = "wasm32")]
pub fn anchored_placement_style(trigger_id: &str, prefer_up: bool) -> Option<String> {
    let window = web_sys::window()?;
    let document = window.document()?;
    let trigger = document.get_element_by_id(trigger_id)?;
    let viewport = viewport_box(&window);
    let containing = containing_block_box(&window, &trigger).unwrap_or(viewport);
    let placement = place_menu(client_box_of(&trigger), viewport, containing, prefer_up);
    Some(position_longhands(&placement))
}

#[cfg(not(target_arch = "wasm32"))]
pub fn anchored_placement_style(_trigger_id: &str, _prefer_up: bool) -> Option<String> {
    None
}

/// The four position longhands of a computed placement. `MenuPlacement::style` also emits
/// `max-height` and `overflow-y: auto`, which would clip a dialog's own focus ring, so they are omitted.
#[cfg(target_arch = "wasm32")]
fn position_longhands(placement: &MenuPlacement) -> String {
    let (top, bottom) = match placement.vertical {
        Vertical::Top(top) => (format!("{top}px"), "auto".to_owned()),
        Vertical::Bottom(bottom) => ("auto".to_owned(), format!("{bottom}px")),
    };
    let (left, right) = match placement.horizontal {
        Horizontal::Left(left) => (format!("{left}px"), "auto".to_owned()),
        Horizontal::Right(right) => ("auto".to_owned(), format!("{right}px")),
    };
    format!("top: {top}; bottom: {bottom}; left: {left}; right: {right};")
}

/// Moves keyboard focus to the element with id `element_id` when it is HTML.
#[cfg(target_arch = "wasm32")]
pub fn focus_element_by_id(element_id: &str) {
    use wasm_bindgen::JsCast;
    let element = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(element_id))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok());
    if let Some(element) = element {
        element.focus().ok();
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn focus_element_by_id(_element_id: &str) {}

#[cfg(target_arch = "wasm32")]
fn viewport_box(window: &web_sys::Window) -> ClientBox {
    let width = window.inner_width().ok().and_then(|value| value.as_f64()).unwrap_or(0.0);
    let height = window.inner_height().ok().and_then(|value| value.as_f64()).unwrap_or(0.0);
    ClientBox { left: 0.0, top: 0.0, width, height }
}

#[cfg(target_arch = "wasm32")]
fn client_box_of(element: &web_sys::Element) -> ClientBox {
    let rect = element.get_bounding_client_rect();
    ClientBox { left: rect.left(), top: rect.top(), width: rect.width(), height: rect.height() }
}

/// The nearest ancestor that becomes the containing block for `position: fixed`:
/// any ancestor with a non-`none` transform. `None` when no ancestor has one, in
/// which case the viewport is the containing block.
#[cfg(target_arch = "wasm32")]
fn containing_block_box(window: &web_sys::Window, trigger: &web_sys::Element) -> Option<ClientBox> {
    let mut ancestor = trigger.parent_element();
    while let Some(element) = ancestor {
        if has_transform(window, &element) {
            return Some(client_box_of(&element));
        }
        ancestor = element.parent_element();
    }
    None
}

#[cfg(target_arch = "wasm32")]
fn has_transform(window: &web_sys::Window, element: &web_sys::Element) -> bool {
    window
        .get_computed_style(element)
        .ok()
        .flatten()
        .and_then(|style| style.get_property_value("transform").ok())
        .is_some_and(|transform| !transform.is_empty() && transform != "none")
}
