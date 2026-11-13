//! Browser caret rectangles for native text controls.
//! A temporary, invisible mirror uses the control's actual typography, padding,
//! wrapping and scroll offsets; password controls mirror mask cells, not secrets.

use wasm_bindgen::JsCast;

use crate::components::deck::deck_dom::ClientBox;

pub(super) fn caret_box(target: &web_sys::Element) -> Option<ClientBox> {
    if let Some(marker) = target.query_selector("[data-omp-caret]").ok().flatten() {
        let visible = web_sys::window()
            .and_then(|window| window.get_computed_style(&marker).ok().flatten())
            .and_then(|style| style.get_property_value("visibility").ok())
            .is_some_and(|visibility| visibility != "hidden");
        let rect = marker.get_bounding_client_rect();
        if visible && rect.height() > 0.0 {
            return Some(ClientBox {
                left: rect.left(),
                top: rect.top(),
                width: rect.width(),
                height: rect.height(),
            });
        }
    }
    let control = if target.matches("input,textarea").ok()? {
        target.clone()
    } else {
        target.query_selector("input,textarea").ok().flatten()?
    };
    let (value, cursor, single_line) = if let Some(input) =
        control.dyn_ref::<web_sys::HtmlInputElement>()
    {
        let value = input.value();
        let cursor = if input.selection_direction().ok().flatten().as_deref() == Some("backward") {
            input.selection_start().ok().flatten()?
        } else {
            input.selection_end().ok().flatten()?
        };
        let value = if input.type_() == "password" {
            "•".repeat(value.encode_utf16().count())
        } else {
            value
        };
        (value, cursor, true)
    } else {
        let textarea = control.dyn_ref::<web_sys::HtmlTextAreaElement>()?;
        let cursor = if textarea.selection_direction().ok().flatten().as_deref() == Some("backward")
        {
            textarea.selection_start().ok().flatten()?
        } else {
            textarea.selection_end().ok().flatten()?
        };
        (textarea.value(), cursor, false)
    };
    let window = web_sys::window()?;
    let document = window.document()?;
    let computed = window.get_computed_style(&control).ok().flatten()?;
    let mirror = document
        .create_element("div")
        .ok()?
        .dyn_into::<web_sys::HtmlElement>()
        .ok()?;
    let style = mirror.style();
    for property in [
        "box-sizing",
        "font-family",
        "font-size",
        "font-style",
        "font-weight",
        "font-variant",
        "font-stretch",
        "line-height",
        "letter-spacing",
        "word-spacing",
        "text-indent",
        "text-transform",
        "text-align",
        "direction",
        "tab-size",
        "padding-top",
        "padding-right",
        "padding-bottom",
        "padding-left",
        "border-top-width",
        "border-right-width",
        "border-bottom-width",
        "border-left-width",
        "border-style",
        "overflow-wrap",
        "word-break",
    ] {
        if let Ok(value) = computed.get_property_value(property) {
            let _ = style.set_property(property, &value);
        }
    }
    let rect = control.get_bounding_client_rect();
    let html = control.dyn_ref::<web_sys::HtmlElement>()?;
    let scale_x = rect.width() / f64::from(html.offset_width().max(1));
    let scale_y = rect.height() / f64::from(html.offset_height().max(1));
    let border_width = ["border-left-width", "border-right-width"]
        .into_iter()
        .filter_map(|property| computed.get_property_value(property).ok())
        .filter_map(|value| value.trim_end_matches("px").parse::<f64>().ok())
        .sum::<f64>();
    let mirror_width = f64::from(control.client_width()) + border_width;
    if single_line {
        let pixels = |property: &str| {
            computed
                .get_property_value(property)
                .ok()
                .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
        };
        if let (Some(top), Some(bottom), Some(line)) = (
            pixels("padding-top"),
            pixels("padding-bottom"),
            pixels("line-height"),
        ) {
            let extra = (f64::from(control.client_height()) - top - bottom - line).max(0.0) / 2.0;
            let _ = style.set_property("padding-top", &format!("{}px", top + extra));
        }
    }
    for (property, value) in [
        ("position", "fixed".to_owned()),
        ("box-sizing", "border-box".to_owned()),
        ("visibility", "hidden".to_owned()),
        ("pointer-events", "none".to_owned()),
        ("left", format!("{}px", rect.left())),
        ("top", format!("{}px", rect.top())),
        ("width", format!("{mirror_width}px")),
        (
            "white-space",
            if single_line { "pre" } else { "pre-wrap" }.to_owned(),
        ),
        ("overflow-wrap", "break-word".to_owned()),
    ] {
        let _ = style.set_property(property, &value);
    }
    let units: Vec<u16> = value.encode_utf16().collect();
    let cursor = usize::try_from(cursor).ok()?.min(units.len());
    let prefix = String::from_utf16_lossy(&units[..cursor]);
    let suffix = String::from_utf16_lossy(&units[cursor..]);
    mirror
        .append_child(&document.create_text_node(&prefix))
        .ok()?;
    let marker = document.create_element("span").ok()?;
    marker.set_text_content(Some("\u{200b}"));
    mirror.append_child(&marker).ok()?;
    mirror
        .append_child(&document.create_text_node(&suffix))
        .ok()?;
    document.body()?.append_child(&mirror).ok()?;
    let caret = marker.get_bounding_client_rect();
    let measured = ClientBox {
        left: rect.left()
            + (caret.left() - rect.left() - f64::from(control.scroll_left())) * scale_x,
        top: rect.top() + (caret.top() - rect.top() - f64::from(control.scroll_top())) * scale_y,
        width: 0.0,
        height: caret.height() * scale_y,
    };
    mirror.remove();
    Some(measured)
}
