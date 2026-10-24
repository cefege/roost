//! The multiline paste confirmation, shown by `CellTerminal` only while a paste
//! is pending. Libero owns modal focus, dismissal and result delivery.

use std::cell::RefCell;
use std::rc::Rc;

use crate::components::overlay_generation::OverlayGeneration;
use dioxus::prelude::*;
use libero::components::{Button, Dialog, Text};
use libero::hooks::{ModalScope, use_modal};
use roost_protocol::terminal_input::count_line_breaks;

/// The confirmation for `text`.
#[component]
pub fn TerminalPasteGuard(
    text: String,
    on_cancel: EventHandler<()>,
    on_send: EventHandler<String>,
) -> Element {
    let generations = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let modal = use_modal(|scope: ModalScope<String, bool>| {
        let Some(text) = scope.try_args() else {
            return rsx! {};
        };
        let lines = count_line_breaks(&text) + 1;
        rsx! {
            Dialog { title: "Paste multiple lines?", role: "alertdialog",
                Text {
                    color: "text-dimmed",
                    "This shell has bracketed paste off, so all {lines} lines run as they arrive — you will not get a chance to edit them first."
                }
                Button {
                    variant: "outlined",
                    "data-testid": "paste-guard-cancel",
                    onclick: move |_| scope.close(),
                    "Cancel"
                }
                Button {
                    "data-testid": "paste-guard-send",
                    onclick: move |_| scope.resolve(true),
                    "Paste {lines} lines"
                }
            }
        }
    });
    use_effect(use_reactive((&text,), {
        let generations = generations.clone();
        move |(text,)| {
            let generation = generations.borrow_mut().begin();
            let current = generations.clone();
            modal.open_with(text.clone()).onresult(move |result| {
                if !current.borrow_mut().finish(generation) {
                    return;
                }
                match result {
                    Some(true) => on_send.call(text.clone()),
                    _ => on_cancel.call(()),
                }
            });
        }
    }));
    use_drop({
        let generations = generations.clone();
        move || generations.borrow_mut().invalidate()
    });
    rsx! {}
}
