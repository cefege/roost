//! Measures controls sharing the composer's field row for `pane_geometry_dom`.
//! A taller microphone contributes to the resting row, not to draft growth.
//! Reads actual CSS grid placement, including items inside `display: contents`.

use wasm_bindgen::JsCast as _;
use web_sys::{Element, HtmlElement, Window};

pub(super) fn control_height_in_field_row(box_element: &Element, input: &Element) -> Option<f64> {
    let window = web_sys::window()?;
    let field = input.parent_element()?;
    let row = window
        .get_computed_style(&field)
        .ok()??
        .get_property_value("grid-row-start")
        .ok()?;
    control_height(&window, box_element, &field, &row)
}

fn control_height(window: &Window, container: &Element, field: &Element, row: &str) -> Option<f64> {
    let children = container.children();
    let mut height: f64 = 0.0;
    for idx in 0..children.length() {
        let child = children.item(idx)?;
        if child == *field {
            continue;
        }
        let style = window.get_computed_style(&child).ok()??;
        if style.get_property_value("display").ok()? == "contents" {
            height = height.max(control_height(window, &child, field, row)?);
        } else if style.get_property_value("grid-row-start").ok()? == row
            && !matches!(
                style.get_property_value("position").ok()?.as_str(),
                "absolute" | "fixed"
            )
        {
            height = height.max(f64::from(child.dyn_ref::<HtmlElement>()?.offset_height()));
        }
    }
    Some(height)
}
