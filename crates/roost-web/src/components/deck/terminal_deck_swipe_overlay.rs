//! The phone's forward-swipe affordance: the new-terminal surface peeking
//! from behind the shrinking terminal, and the + FAB that grows under the
//! finger and blooms into the new terminal on commit. Rendered once by
//! `TerminalDeck`; the geometry is `deck_swipe_style`. Ports
//! `apps/web/src/components/deck/TerminalDeckSwipeOverlay.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::PaneRect;

use super::deck_swipe::{Swipe, SwipeMode, new_fab_progress};
use super::deck_swipe_style::{new_fab_style, new_peek_style};
use crate::components::ui::{Icon, IconType};

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
            class: "flex items-center justify-center overflow-hidden bg-card [-webkit-tap-highlight-color:transparent]",
            "data-testid": "deck-new-peek",
            style: new_peek_style(Some(pull), pane_rect, deck_width, strip_height).css(),
            "aria-hidden": "true",
            div { class: "flex items-center gap-2.5 font-medium text-sm leading-tight tracking-[0.1px] text-muted-foreground",
                Icon { icon: IconType::Plus, class: "size-5 [stroke-width:2.5]" }
                span { "New terminal · {folder_label}" }
            }
        }
        div {
            class: "flex items-center justify-center bg-secondary text-secondary-foreground shadow-[0_6px_20px_color-mix(in_srgb,black_45%,transparent)] transition-[background-color,color,box-shadow] duration-150 ease-[cubic-bezier(0.2,0,0,1)] [-webkit-tap-highlight-color:transparent] data-[armed=true]:bg-primary data-[armed=true]:text-primary-foreground data-[armed=true]:shadow-[0_8px_26px_color-mix(in_srgb,black_50%,transparent),0_0_0_12px_color-mix(in_srgb,var(--accent)_18%,transparent)]",
            "data-testid": "deck-new-fab",
            "data-armed": armed.then_some("true"),
            style: new_fab_style(Some(pull), pane_rect, deck_width, strip_height).css(),
            "aria-hidden": "true",
            Icon { icon: IconType::Plus, class: "size-7.5 [stroke-width:2.5]" }
        }
    }
}
