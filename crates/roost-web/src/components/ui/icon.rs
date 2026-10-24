//! `Icon`: a Lucide glyph drawn as inline SVG from the `icons` crate's path
//! registry (rust-ui's icon set). Composed by every `ui` primitive and surface
//! that shows a glyph; depends only on `icons` and `tw_merge`.
//!
//! The crate's own `DioxusIcon` titles every glyph "Rust/UI Icons - <name>",
//! which every hover would show as a tooltip; this draws the same elements
//! with no title, `aria-hidden`, because the accessible name belongs to the
//! control around the glyph.

use dioxus::prelude::*;
use icons::common::IconType;
use icons::common::{StaticSvgElement, icon_registry_getter::get_icon_elements};
use tw_merge::tw_merge;

/// A decorative Lucide glyph, `size-4` unless `class` sizes it.
#[component]
pub fn Icon(icon: IconType, #[props(into, optional)] class: Option<String>) -> Element {
    let class = tw_merge!("size-4 shrink-0", class.as_deref().unwrap_or(""));
    let elements = get_icon_elements(icon).unwrap_or(&[]);
    rsx! {
        svg {
            xmlns: "http://www.w3.org/2000/svg",
            view_box: "0 0 24 24",
            fill: "none",
            stroke: "currentColor",
            "stroke-width": "2",
            "stroke-linecap": "round",
            "stroke-linejoin": "round",
            "aria-hidden": "true",
            "data-icon": "{icon}",
            class,
            for element in elements.iter() {
                {svg_element(element)}
            }
        }
    }
}

fn svg_element(element: &StaticSvgElement) -> Element {
    match element {
        StaticSvgElement::Path { d } => rsx! { path { d: *d } },
        StaticSvgElement::Circle { cx, cy, r } => rsx! {
            circle { cx: *cx, cy: *cy, r: *r }
        },
        StaticSvgElement::Rect {
            x,
            y,
            width,
            height,
            rx: Some(rx),
            ry: Some(ry),
        } => rsx! {
            rect { x: *x, y: *y, width: *width, height: *height, rx: *rx, ry: *ry }
        },
        StaticSvgElement::Rect {
            x,
            y,
            width,
            height,
            rx: Some(rx),
            ry: None,
        } => rsx! {
            rect { x: *x, y: *y, width: *width, height: *height, rx: *rx }
        },
        StaticSvgElement::Rect {
            x,
            y,
            width,
            height,
            rx: None,
            ry: Some(ry),
        } => rsx! {
            rect { x: *x, y: *y, width: *width, height: *height, ry: *ry }
        },
        StaticSvgElement::Rect {
            x,
            y,
            width,
            height,
            rx: None,
            ry: None,
        } => rsx! {
            rect { x: *x, y: *y, width: *width, height: *height }
        },
        StaticSvgElement::Ellipse { cx, cy, rx, ry } => rsx! {
            ellipse { cx: *cx, cy: *cy, rx: *rx, ry: *ry }
        },
        StaticSvgElement::Line { x1, y1, x2, y2 } => rsx! {
            line { x1: *x1, y1: *y1, x2: *x2, y2: *y2 }
        },
        StaticSvgElement::Polyline { points } => rsx! {
            polyline { points: *points }
        },
        StaticSvgElement::Polygon { points } => rsx! {
            polygon { points: *points }
        },
    }
}
