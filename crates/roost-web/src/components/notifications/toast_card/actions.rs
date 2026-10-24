//! Actions for a Roost notification without a second dismissal queue.
//! The card supplies existing reveal, clipboard-result and dismiss handlers.
//! Libero supplies button sizing, names and focus behavior.

use dioxus::prelude::*;
use libero::components::{ActionIcon, Button};

#[component]
pub(super) fn ToastActionRow(
    label: Option<String>,
    show_copy: bool,
    copied: bool,
    compact: bool,
    on_reveal: EventHandler<MouseEvent>,
    on_copy: EventHandler<MouseEvent>,
    on_dismiss: EventHandler<MouseEvent>,
) -> Element {
    let size = if compact { "sm" } else { "xs" };
    rsx! {
        if let Some(label) = label.as_ref() {
            Button { variant: "standard", size, title: label.clone(),
                onclick: move |event: MouseEvent| { event.stop_propagation(); on_reveal.call(event); },
                "{label}"
            }
        }
        if show_copy {
            Button { variant: "standard", size,
                title: if copied { "Copied to clipboard" } else { "Copy full message" },
                onclick: move |event: MouseEvent| { event.stop_propagation(); on_copy.call(event); },
                if copied { "Copied" } else { "Copy" }
            }
        }
        ActionIcon { icon: pictogram_icons_lucide::x::outlined,
            aria_label: "Dismiss", size, variant: "standard",
            onclick: move |event: MouseEvent| { event.stop_propagation(); on_dismiss.call(event); },
        }
    }
}
