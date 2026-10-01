//! The transient controller legend: which button does what on the surface that
//! currently holds focus. A dock child, so it inherits the bottom overlay
//! geometry instead of claiming its own corner. `aria-hidden` plus
//! `pointer-events: none` keep it out of focus ownership entirely — a screen
//! reader gets the same catalogue from the help surface's controller rows.
//! Ports `apps/web/src/components/notifications/PadHintBar.tsx` over
//! `input_nav::pad_hints`, the one binding table the controller router also
//! reads, so the legend and the actions cannot drift apart.

use dioxus::prelude::*;

use crate::components::md::{BindingChip, Surface, SurfaceRadius};
use crate::input_nav::modality::NavModality;
use crate::input_nav::pad_hints::{PadHintContext, pad_hints};

/// The legend, or nothing off a controller.
///
/// The context is the DEFAULT one because this dock is mounted at the shell's
/// root, outside every surface-specific focus region; the controller router
/// raises the legend and expires it, and a legend with no router behind it has
/// no surface to be specific to.
#[component]
pub fn PadHintBar() -> Element {
    let Some(modality) = try_use_context::<Signal<NavModality>>() else {
        return rsx! {};
    };
    if !modality().pad_mode_active() {
        return rsx! {};
    }
    let hints = pad_hints(PadHintContext::Default);
    rsx! {
        Surface {
            level: 2,
            elevation: 3,
            radius: SurfaceRadius::Md,
            test_id: Some("pad-hint-bar".to_owned()),
            aria_hidden: Some("true".to_owned()),
            class: Some("pad-hint".to_owned()),
            style: Some("pointer-events: none;".to_owned()),
            for hint in hints {
                span {
                    class: "pad-hint__item",
                    BindingChip { "{hint.cap}" }
                    span { class: "md-label-s pad-hint__label", "{hint.label}" }
                }
            }
        }
    }
}
