//! The spotlight's two chrome layers: the scrim that dims the tiled stack
//! (a press or right-click on it puts the pane back) and the card frame
//! around the floated pane. The floated terminal itself is an ordinary deck
//! slot. Rendered once by `TerminalDeck`. Ports
//! `apps/web/src/components/deck/TerminalDeckSpotlight.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::PaneRect;

use super::deck_dom;
use super::inline_style::px;

/// The scrim and card for `rect`, or nothing when no pane is floated.
#[component]
pub fn TerminalDeckSpotlight(rect: Option<PaneRect>, on_dismiss: EventHandler<()>) -> Element {
    let Some(rect) = rect else {
        return rsx! {};
    };
    // Reduced motion paints the scrim's end state on its first frame; read at
    // the moment the spotlight opens, which is when the fade would start.
    let animation = if deck_dom::reduced_motion() {
        " animation: none;"
    } else {
        ""
    };
    rsx! {
        div {
            class: "pane-spotlight-backdrop",
            "data-testid": "pane-spotlight-backdrop",
            style: "position: absolute; inset: 0; z-index: 7;{animation}",
            "aria-hidden": "true",
            onpointerdown: move |event: PointerEvent| {
                event.stop_propagation();
                on_dismiss.call(());
            },
            oncontextmenu: move |event: MouseEvent| {
                event.prevent_default();
                on_dismiss.call(());
            },
        }
        div {
            class: "pane-spotlight-card",
            style: "position: absolute; left: {px(rect.x)}; top: {px(rect.y)}; width: {px(rect.w)}; height: {px(rect.h)}; z-index: 8; pointer-events: none;",
            "aria-hidden": "true",
        }
    }
}
