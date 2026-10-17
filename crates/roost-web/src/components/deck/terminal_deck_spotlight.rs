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
            class: "rounded-xl shadow-[0_0_0_1px_var(--border-strong),_0_8px_12px_6px_color-mix(in_srgb,black_15%,transparent),_0_4px_4px_0_color-mix(in_srgb,black_30%,transparent)]",
            style: "position: absolute; left: {px(rect.x)}; top: {px(rect.y)}; width: {px(rect.w)}; height: {px(rect.h)}; z-index: 8; pointer-events: none;",
            "aria-hidden": "true",
        }
    }
}
