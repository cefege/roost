//! A Libero use_popover example for compact session details.

use dioxus::prelude::*;
use libero::{
    components::{Box, Button, Text},
    hooks::{PopoverOptions, Side, use_element, use_id, use_popover},
    sx::sx,
    use_theme,
};

#[component]
pub fn PopoverSpecimen() -> Element {
    let theme = use_theme();
    let mut open = use_signal(|| false);
    let anchor = use_element();
    let content_id = use_id();
    let floating_sx = use_hook(|| {
        sx().background("surface")
            .padding("var(--lsx-popover-padding)")
    });
    let popover = use_popover(
        anchor,
        open(),
        PopoverOptions::new(theme.popover.gap, theme.popover.padding)
            .side(Side::Bottom)
            .dismiss(true),
    );
    popover.on_dismiss(move || open.set(false));
    popover.show(open().then(|| {
        rsx! {
            Box {
                id: "{content_id}",
                role: "tooltip",
                attributes: popover.floating_events(),
                style: popover.style(),
                onmounted: (*popover.floating()).mount(),
                sx: floating_sx,
                Text { "ovh1 · connected · coordinator link healthy" }
            }
        }
    }));

    rsx! {
        Button {
            onmounted: anchor.mount(),
            onclick: move |_| open.toggle(),
            aria_expanded: open(),
            aria_controls: "{content_id}",
            attributes: popover.anchor_events(),
            "Show popover details"
        }
    }
}
