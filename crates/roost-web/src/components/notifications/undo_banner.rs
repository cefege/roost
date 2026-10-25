//! Stock toast presentation for each terminal's pending close.
//! The client-core queue owns deadlines; Undo dispatches through the deck
//! so both the session row and its original pane arrangement are restored.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::DeckIntent;

use crate::components::ui::{Button, ButtonSize, ButtonVariant, ToastPresentation};
use crate::pump::use_store;

/// One card per close that is still inside its window.
#[component]
pub fn UndoCloseBanner() -> Element {
    let pump = use_store();
    let entries = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .pending_closes
            .entries()
            .cloned()
            .collect::<Vec<_>>()
    };
    rsx! {
        for entry in entries {
            UndoSnackbar { entry: entry }
        }
    }
}

/// One close's card, with the way back.
#[component]
fn UndoSnackbar(entry: roost_client_core::store::pending_close::PendingClose) -> Element {
    let pump = use_store();
    let session_id = entry.session_id.clone();
    let sub = [entry.labels.folder.as_str(), entry.labels.server.as_str()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    // The DECK's undo, not the queue's: a close took a tab out of a pane, and
    // only `DeckIntent::UndoClose` puts it back where it was. The queue's
    // `undo_one` un-hides the row and nothing else, and this card is also the
    // one the SIDEBAR's close raises a card for, so it cannot know which pane
    // arrangement to restore without asking the deck that recorded it.
    let undo = move |_event: MouseEvent| {
        pump.dispatch(ClientEvent::Deck(DeckIntent::UndoClose {
            session_id: session_id.clone(),
        }));
    };
    rsx! {
        div {
            "data-testid": "undo-close-banner",
            "data-session-id": entry.session_id.clone(),
            ToastPresentation {
                class: "relative w-full max-w-full min-w-0 cursor-default touch-auto overflow-hidden",
                title: format!("{} closed", entry.labels.terminal_name),
                description: (!sub.is_empty()).then_some(sub),
                role: "status",
                aria_live: "polite",
                progress: Some(1.0),
                progress_class: "animate-roost-countdown-drain motion-reduce:animate-none",
                progress_style: "animation-duration: var(--roost-undo-window-ms, 5000ms);",
                div { class: "mt-3 flex items-center gap-2",
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        "data-testid": "undo-snackbar-action",
                        onclick: undo,
                        "Undo"
                    }
                }
            }
        }
    }
}
