//! The undo snackbar a soft-closed terminal waits behind: one dark card per
//! pending close, the closed terminal's name over a dimmer folder · machine
//! line, and the way back. Each card owns its window independently — the queue
//! and its deadlines are `roost_client_core::store::pending_close`, and the
//! card's Undo raises the deck's `UndoClose`, which is what puts a closed tab
//! back in its pane. Ports `apps/web/src/components/notifications/UndoCloseBanner.tsx`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::DeckIntent;

use crate::components::md::{Button, ButtonSize, ButtonVariant, Surface, SurfaceRadius};
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
            Surface {
                level: 3,
                elevation: 4,
                radius: SurfaceRadius::Md,
                style: "position: relative; display: flex; align-items: center; gap: var(--md-space-3); color: var(--md-sys-color-on-surface); padding: var(--md-space-3) var(--md-space-2) var(--md-space-3) var(--md-space-4); overflow: hidden;".to_owned(),
                div {
                    style: "flex: 1; min-width: 0; display: flex; flex-direction: column; gap: var(--md-space-1);",
                    span {
                        "data-testid": "undo-snackbar-text",
                        class: "md-body-m",
                        style: "white-space: nowrap; overflow: hidden; text-overflow: ellipsis;",
                        span { class: "md-label-l", "{entry.labels.terminal_name}" }
                        " closed"
                    }
                    if !sub.is_empty() {
                        span {
                            "data-testid": "undo-snackbar-sub",
                            class: "md-body-s",
                            style: "white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: var(--md-sys-color-on-surface-variant);",
                            "{sub}"
                        }
                    }
                }
                Button {
                    variant: ButtonVariant::Ghost,
                    size: ButtonSize::Sm,
                    "data-testid": "undo-snackbar-action",
                    onclick: undo,
                    "Undo"
                }
                span {
                    "aria-hidden": "true",
                    style: "position: absolute; inset: auto 0 0; height: var(--workbench-border-width); background: var(--md-sys-color-primary); transform-origin: left center; animation: undo-snackbar-bar var(--roost-undo-window-ms, 5000ms) linear forwards;".to_owned(),
                }
            }
        }
    }
}
