//! Shown by `CellTerminal` over a VIEWED pane whose view stayed undeliverable
//! through its silent re-claims — a dead "breadcrumb" session. Replaces the
//! silent blank pane with an explicit state and two escape hatches; the wrapper
//! is click-through so only the card is interactive.

use dioxus::prelude::*;

use crate::components::ui::{Button, ButtonSize, ButtonVariant, Card, CardDescription, CardTitle};

/// The notice.
#[component]
pub fn TerminalOfflineNotice(
    on_retry: EventHandler<()>,
    on_open_sibling: EventHandler<()>,
    has_sibling: bool,
) -> Element {
    rsx! {
        div {
            "data-testid": "terminal-offline-notice",
            class: "absolute inset-0 z-[5] flex items-center justify-center p-6 pointer-events-none",
            // A discrete state change, safe to announce; the grid itself never
            // gets a live region, or a streaming pane floods the reader.
            Card {
                class: Some(
                    "pointer-events-auto w-[min(100%,45ch)] gap-3 rounded-lg p-6 text-center shadow-md"
                        .to_owned(),
                ),
                "aria-live": "polite",
                CardTitle { class: "text-base font-medium", "This terminal isn't responding" }
                CardDescription {
                    "Its process may have stopped. The tab stays put so you keep your place."
                }
                div {
                    class: "mt-1 flex flex-wrap justify-center gap-2",
                    Button {
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        class: None,
                        "data-testid": "terminal-offline-retry",
                        onclick: move |_| on_retry.call(()),
                        "Retry"
                    }
                    if has_sibling {
                        Button {
                            variant: ButtonVariant::Default,
                            size: ButtonSize::Sm,
                            class: None,
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
