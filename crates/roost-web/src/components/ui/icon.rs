//! `Icon`: a Lucide glyph drawn as inline SVG from the `icons` crate's path
//! registry (rust-ui's icon set). Composed by every `ui` primitive and surface
//! that shows a glyph; depends only on `icons` and `tw_merge`.
//!
//! The crate's own `DioxusIcon` titles every glyph "Rust/UI Icons - <name>",
//! which every hover would show as a tooltip; this draws the same elements
//! with no title, `aria-hidden`, because the accessible name belongs to the
//! control around the glyph.

use dioxus::prelude::*;
use icons::IconType;
use icons::common::icon_registry_getter::get_icon_elements;
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
                {element.to_dioxus_element()}
            }
        }
    }
}
