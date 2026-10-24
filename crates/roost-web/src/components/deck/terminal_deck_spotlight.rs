//! The spotlight's two chrome layers: the scrim that dims the tiled stack
//! (a press or right-click on it puts the pane back) and the card frame
//! around the floated pane. The floated terminal itself is an ordinary deck
//! slot. Rendered once by `TerminalDeck`. Ports
//! `apps/web/src/components/deck/TerminalDeckSpotlight.tsx`.

use dioxus::prelude::*;
use libero::components::Box;
use libero::sx::sx;
use roost_client_core::store::layout::PaneRect;

use super::inline_style::px;

/// The scrim and card for `rect`, or nothing when no pane is floated.
#[component]
pub fn TerminalDeckSpotlight(rect: Option<PaneRect>, on_dismiss: EventHandler<()>) -> Element {
    let Some(rect) = rect else {
        return rsx! {};
    };
    let backdrop_sx = use_hook(|| {
        sx().position("absolute")
            .inset("0")
            .with("z-index", "7")
            .background("ink")
            .opacity("0.55")
            .with(
                "animation",
                "pane-spotlight-fade 120ms cubic-bezier(0.3, 0, 1, 1)",
            )
            .media("(prefers-reduced-motion: reduce)", sx().animation("none"))
    });
    let frame_sx = use_hook(|| {
        sx().border_radius("xl")
            .box_shadow(libero::theme::SizeCss::SHADOW.value(libero::theme::Size::Xl))
    });
    rsx! {
        Box {
            component: "div",
            sx: backdrop_sx,
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
        Box {
            component: "div",
            sx: frame_sx,
            style: "position: absolute; left: {px(rect.x)}; top: {px(rect.y)}; width: {px(rect.w)}; height: {px(rect.h)}; z-index: 8; pointer-events: none;",
            "aria-hidden": "true",
        }
    }
}
