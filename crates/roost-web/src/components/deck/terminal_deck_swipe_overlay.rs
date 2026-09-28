//! The phone's forward-swipe affordance: the new-terminal surface peeking
//! from behind the shrinking terminal, and the + FAB that grows under the
//! finger and blooms into the new terminal on commit. Rendered once by
//! `TerminalDeck`; the geometry is `deck_swipe_style`. Ports
//! `apps/web/src/components/deck/TerminalDeckSwipeOverlay.tsx`.

use dioxus::prelude::*;
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
    let armed = new_fab_progress(pull.offset, deck_width) >= 1.0;
    rsx! {
        div {
            class: "deck-new-peek",
            "data-testid": "deck-new-peek",
            style: new_peek_style(Some(pull), pane_rect, deck_width, strip_height).css(),
            "aria-hidden": "true",
            div { class: "deck-new-peek__label",
                svg { width: "20", height: "20", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2.5", stroke_linecap: "round", "aria-hidden": "true",
                    path { d: "M12 5v14M5 12h14" }
                }
                span { "New terminal · {folder_label}" }
            }
        }
        div {
            class: "deck-new-fab",
            "data-testid": "deck-new-fab",
            "data-armed": armed.then_some("true"),
            style: new_fab_style(Some(pull), pane_rect, deck_width, strip_height).css(),
            "aria-hidden": "true",
            svg { width: "30", height: "30", view_box: "0 0 24 24", fill: "none", stroke: "currentColor", stroke_width: "2.5", stroke_linecap: "round", "aria-hidden": "true",
                path { d: "M12 5v14M5 12h14" }
            }
        }
    }
}
