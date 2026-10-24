//! Shown by `CellTerminal` over a VIEWED pane whose view stayed undeliverable
//! through its silent re-claims — a dead "breadcrumb" session. Replaces the
//! silent blank pane with an explicit state and two escape hatches; the wrapper
//! is click-through so only the card is interactive.

use dioxus::prelude::*;
use libero::components::{Box, Button, Paper, Text, Title};
use libero::sx::sx;
/// The notice.
#[component]
pub fn TerminalOfflineNotice(
    on_retry: EventHandler<()>,
    on_open_sibling: EventHandler<()>,
    has_sibling: bool,
) -> Element {
    let overlay_sx = use_hook(|| {
        sx().position("absolute")
            .with("inset", "0")
            .z_index("5")
            .display("flex")
            .align_items("center")
            .justify_content("center")
            .padding("lg")
            .pointer_events("none")
    });
    let paper_sx = use_hook(|| {
        sx().pointer_events("auto")
            .width("min(100%, 45ch)")
            .gap("sm")
            .padding("lg")
            .text_align("center")
    });
    let actions_sx = use_hook(|| {
        sx().margin_top("xs")
            .display("flex")
            .flex_wrap("wrap")
            .justify_content("center")
            .gap("sm")
    });
    rsx! {
        Box {
            component: "div",
            sx: overlay_sx,
            "data-testid": "terminal-offline-notice",
            // A discrete state change, safe to announce; the grid itself never
            // gets a live region, or a streaming pane floods the reader.
            Paper {
                shadow: "md",
                sx: paper_sx,
                "aria-live": "polite",
                Title { component: "h2", size: "sm", "This terminal isn't responding" }
                Text { color: "text-dimmed", "Its process may have stopped. The tab stays put so you keep your place." }
                Box { component: "div", sx: actions_sx,
                    Button {
                        variant: "outlined",
                        size: "sm",
                        "data-testid": "terminal-offline-retry",
                        onclick: move |_| on_retry.call(()),
                        "Retry"
                    }
                    if has_sibling {
                        Button {
                            "data-testid": "terminal-offline-open-sibling",
                            onclick: move |_| on_open_sibling.call(()),
                            "Open another terminal here"
                        }
                    }
                }
            }
        }
    }
}
