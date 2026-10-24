//! The phone's forward-swipe affordance: the new-terminal surface peeking
//! from behind the shrinking terminal, and the + FAB that grows under the
//! finger and blooms into the new terminal on commit. Rendered once by
//! `TerminalDeck`; the geometry is `deck_swipe_style`. Ports
//! `apps/web/src/components/deck/TerminalDeckSwipeOverlay.tsx`.

use dioxus::prelude::*;
use libero::components::{Box, Pictogram};
use libero::sx::sx;
use roost_client_core::store::layout::PaneRect;

use super::deck_swipe::{Swipe, SwipeMode, new_fab_progress};
use super::deck_swipe_style::{new_fab_style, new_peek_style};

/// The overlay, painted only on a compact new-terminal pull.
#[component]
pub fn TerminalDeckSwipeOverlay(
    compact: bool,
    swipe: Option<Swipe>,
    pane_rect: Option<PaneRect>,
    deck_width: f64,
    strip_height: f64,
    folder_label: String,
) -> Element {
    let Some(pull) = swipe
        .as_ref()
        .filter(|swipe| compact && swipe.mode == SwipeMode::NewTerminal)
    else {
        return rsx! {};
    };
    let peek_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .justify_content("center")
            .overflow("hidden")
            .background("surface")
            .with("-webkit-tap-highlight-color", "transparent")
    });
    let label_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .gap("sm")
            .font_weight("500")
            .font_size("sm")
            .line_height("1.25")
            .color("muted")
    });
    let fab_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("row")
            .align_items("center")
            .justify_content("center")
            .background("secondary")
            .color("ink")
            .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Lg))
            .transition("background-color 150ms, color 150ms, box-shadow 150ms")
            .with("transition-timing-function", "cubic-bezier(0.2, 0, 0, 1)")
            .with("-webkit-tap-highlight-color", "transparent")
            .selector(
                "&[data-armed=true]",
                sx().background("primary")
                    .color("ink")
                    .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Xl)),
            )
    });
    let armed = new_fab_progress(pull.offset, deck_width) >= 1.0;
    rsx! {
        Box {
            component: "div",
            sx: peek_sx,
            "data-testid": "deck-new-peek",
            style: new_peek_style(Some(pull), pane_rect, deck_width, strip_height).css(),
            "aria-hidden": "true",
            Box { component: "div", sx: label_sx,
                Pictogram { icon: pictogram_icons_lucide::plus::outlined, width: "1.25rem", height: "1.25rem" }
                span { "New terminal · {folder_label}" }
            }
        }
        Box {
            component: "div",
            sx: fab_sx,
            "data-testid": "deck-new-fab",
            "data-armed": armed.then_some("true"),
            style: new_fab_style(Some(pull), pane_rect, deck_width, strip_height).css(),
            "aria-hidden": "true",
            Pictogram { icon: pictogram_icons_lucide::plus::outlined, width: "1.875rem", height: "1.875rem" }
        }
    }
}
