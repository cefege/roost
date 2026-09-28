//! `Chip`: a compact label, or a compact action when the caller gives it one.
//! Ported from `apps/web/src/components/Settings/md/Chip.tsx`; the folder grid,
//! filters and control specimens compose it. `controls.css` owns its look.
//!
//! The host element follows the capability: with an `onclick` it is a real
//! `<button>` with `aria-pressed` when a selection state is given; without one it
//! is a passive `<span>`, so a label never pretends to be a control.

use dioxus::prelude::*;

use super::icon::{Icon, IconSize};

/// `data-selected` is written only when selected, as v2 did, so the stylesheet's
/// `[data-selected="true"]` rule is the one reading.
pub fn chip_selected_attribute(selected: Option<bool>) -> Option<&'static str> {
    (selected == Some(true)).then_some("true")
}

/// A chip.
#[component]
pub fn Chip(
    label: String,
    icon: Option<String>,
    selected: Option<bool>,
    onclick: Option<EventHandler<()>>,
    title: Option<String>,
    test_id: Option<String>,
) -> Element {
    let content = rsx! {
        if let Some(icon) = icon {
            Icon { name: icon, size: IconSize::Sm }
        }
        {label}
    };
    match onclick {
        Some(handler) => rsx! {
            button {
                r#type: "button",
                class: "roost-chip",
                "data-selected": chip_selected_attribute(selected),
                "aria-pressed": selected.map(|selected| selected.to_string()),
                "data-testid": test_id,
                title,
                onclick: move |_| handler.call(()),
                {content}
            }
        },
        None => rsx! {
            span {
                class: "roost-chip",
                "data-selected": chip_selected_attribute(selected),
                "data-testid": test_id,
                title,
                {content}
            }
        },
    }
}
