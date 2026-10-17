//! Shown by `CellTerminal` over a VIEWED pane whose view stayed undeliverable
//! through its silent re-claims — a dead "breadcrumb" session. Replaces the
//! silent blank pane with an explicit state and two escape hatches; the wrapper
//! is click-through so only the card is interactive. Ports
//! `apps/web/src/components/terminal/TerminalOfflineNotice.tsx`.

use dioxus::prelude::*;

use crate::components::ui::{Button, ButtonSize, ButtonVariant};

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
            div {
                class: "pointer-events-auto flex w-[min(100%,45ch)] flex-col gap-3 rounded-lg border border-border bg-card p-6 text-center text-card-foreground shadow-md",
                "aria-live": "polite",
                div { class: "text-base font-medium", "This terminal isn't responding" }
                div {
                    class: "text-sm text-muted-foreground",
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
