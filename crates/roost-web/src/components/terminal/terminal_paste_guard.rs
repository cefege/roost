//! The multiline paste confirmation: mounted by `CellTerminal` only while a
//! paste is pending, because a closed dialog inside the pane would fold its
//! prose into the terminal's text and read as painted output. Ports the
//! paste-guard `Dialog` of `apps/web/src/components/terminal/CellTerminal.tsx`.

use dioxus::prelude::*;
use roost_protocol::terminal_input::count_line_breaks;

use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogBody, DialogContent, DialogFooter, DialogHeader,
    DialogTitle,
};

/// The confirmation for `text`.
#[component]
pub fn TerminalPasteGuard(
    text: String,
    on_cancel: EventHandler<()>,
    on_send: EventHandler<String>,
) -> Element {
    let lines = count_line_breaks(&text) + 1;
    rsx! {
        Dialog {
            open: true,
            on_open_change: move |open: bool| {
                if !open {
                    on_cancel.call(());
                }
            },
            DialogContent {
                DialogHeader {
                    DialogTitle { "Paste multiple lines?" }
                }
                DialogBody {
                    p {
                        class: "m-0 text-muted-foreground",
                        "This shell has bracketed paste off, so all {lines} lines run as they arrive — you will not get a chance to edit them first."
                    }
                }
                DialogFooter {
                    Button {
                        variant: ButtonVariant::Outline,
                        "data-testid": "paste-guard-cancel",
                        onclick: move |_| on_cancel.call(()),
                        "Cancel"
                    }
                    Button {
                        variant: ButtonVariant::Default,
                        "data-testid": "paste-guard-send",
                        onclick: move |_| on_send.call(text.clone()),
                        "Paste {lines} lines"
                    }
                }
            }
        }
    }
}
