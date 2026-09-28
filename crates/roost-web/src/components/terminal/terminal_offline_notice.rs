//! Shown by `CellTerminal` over a VIEWED pane whose view stayed undeliverable
//! through its silent re-claims — a dead "breadcrumb" session. Replaces the
//! silent blank pane with an explicit state and two escape hatches; the wrapper
//! is click-through so only the card is interactive. Ports
//! `apps/web/src/components/terminal/TerminalOfflineNotice.tsx`.

use dioxus::prelude::*;

use crate::components::md::{Button, ButtonSize, ButtonVariant, Surface};

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
            style: "position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; padding: var(--md-space-6); pointer-events: none; z-index: 5;",
            // A discrete state change, safe to announce; the grid itself never
            // gets a live region, or a streaming pane floods the reader.
            Surface {
                level: 1,
                elevation: 2,
                pad: 5,
                border: true,
                aria_live: "polite",
                style: "width: min(100%, 45ch); display: flex; flex-direction: column; gap: var(--md-space-3); color: var(--md-sys-color-on-surface); text-align: center; pointer-events: auto;",
                div { class: "md-title-s", "This terminal isn't responding" }
                div {
                    class: "md-body-m",
                    style: "color: var(--md-sys-color-on-surface-variant);",
                    "Its process may have stopped. The tab stays put so you keep your place."
                }
                div {
                    style: "display: flex; gap: var(--md-space-2); justify-content: center; margin-top: var(--md-space-1); flex-wrap: wrap;",
                    Button {
                        variant: ButtonVariant::Outline,
                        size: ButtonSize::Sm,
                        "data-testid": "terminal-offline-retry",
                        onclick: move |_| on_retry.call(()),
                        "Retry"
                    }
                    if has_sibling {
                        Button {
                            variant: ButtonVariant::Default,
                            size: ButtonSize::Sm,
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
