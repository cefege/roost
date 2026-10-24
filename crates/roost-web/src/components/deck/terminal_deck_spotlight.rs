//! The spotlight's two chrome layers: the scrim that dims the tiled stack
//! (a press or right-click on it puts the pane back) and the card frame
//! around the floated pane. The floated terminal itself is an ordinary deck
//! slot. Rendered once by `TerminalDeck`. Ports
//! `apps/web/src/components/deck/TerminalDeckSpotlight.tsx`.

use dioxus::prelude::*;
use roost_client_core::store::layout::PaneRect;

use super::inline_style::px;

/// The scrim and card for `rect`, or nothing when no pane is floated.
#[component]
pub fn TerminalDeckSpotlight(rect: Option<PaneRect>, on_dismiss: EventHandler<()>) -> Element {
    let Some(rect) = rect else {
        return rsx! {};
    };
    rsx! {
        div {
            class: "absolute inset-0 z-[7] bg-[color-mix(in_srgb,black_55%,transparent)] animate-pane-spotlight-fade motion-reduce:animate-none",
            "data-testid": "pane-spotlight-backdrop",
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
            class: "rounded-xl ring-1 ring-input shadow-lg",
            style: "position: absolute; left: {px(rect.x)}; top: {px(rect.y)}; width: {px(rect.w)}; height: {px(rect.h)}; z-index: 8; pointer-events: none;",
            "aria-hidden": "true",
        }
    }
}
