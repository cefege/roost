//! The transient controller legend: which button does what on the surface that
//! currently holds focus. A dock child, so it inherits the bottom overlay
//! geometry instead of claiming its own corner. `aria-hidden` plus
//! `pointer-events: none` keep it out of focus ownership entirely — a screen
//! reader gets the same catalogue from the help surface's controller rows.
//! Ports `apps/web/src/components/notifications/PadHintBar.tsx` over
//! `input_nav::pad_hints`, the one binding table the controller router also
//! reads, so the legend and the actions cannot drift apart.

use dioxus::prelude::*;

use crate::input_nav::modality::NavModality;
use crate::input_nav::pad_hints::{PadHintContext, pad_hints};
use libero::{
    components::{Flex, Kbd, Paper, Text},
    sx::sx,
};

/// The legend, or nothing off a controller.
///
/// The context is the DEFAULT one because this dock is mounted at the shell's
/// root, outside every surface-specific focus region; the controller router
/// raises the legend and expires it, and a legend with no router behind it has
/// no surface to be specific to.
#[component]
pub fn PadHintBar() -> Element {
    let legend_sx = use_hook(|| {
        sx().pointer_events("none")
            .width("auto")
            .max_width("100%")
            .display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .justify_content("center")
            .gap("sm")
            .padding("xs")
            .with("align-self", "center")
    });
    let label_sx = use_hook(|| sx().white_space("nowrap"));
    let Some(modality) = try_use_context::<Signal<NavModality>>() else {
        return rsx! {};
    };
    if !modality().pad_mode_active() {
        return rsx! {};
    }
    let hints = pad_hints(PadHintContext::Default);
    rsx! {
        Paper {
            shadow: "md", radius: "md", sx: legend_sx,
            class: "pad-hint", "data-testid": "pad-hint-bar", aria_hidden: "true",
            for hint in hints {
                Flex { direction: "row", align: "center", gap: "xs", wrap: false,
                    Kbd { "{hint.cap}" }
                    Text { size: "xs", color: "muted", sx: label_sx.clone(), "{hint.label}" }
                }
            }
        }
    }
}
