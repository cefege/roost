//! The undo snackbar a soft-closed terminal waits behind: one dark card per
//! pending close, the closed terminal's name over a dimmer folder · machine
//! line, and the way back. Each card owns its window independently — the queue
//! and its deadlines are `roost_client_core::store::pending_close`, and the
//! card's Undo raises the deck's `UndoClose`, which is what puts a closed tab
//! back in its pane. Ports `apps/web/src/components/notifications/UndoCloseBanner.tsx`.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::deck::DeckIntent;

use crate::pump::use_store;
use libero::{
    components::{Box, Button, Flex, Paper, Text},
    sx::sx,
};

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
    let card_sx = use_hook(|| {
        sx().position("relative")
            .display("flex")
            .align_items("center")
            .gap("sm")
            .overflow("hidden")
            .padding("sm")
    });
    let label_sx = use_hook(|| sx().min_width("0").flex("1"));
    let truncate_sx = use_hook(|| {
        sx().overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .white_space("nowrap")
            .with("text-overflow", "ellipsis")
    });
    let countdown_sx = use_hook(|| {
        sx().position("absolute")
            .with("left", "0")
            .with("right", "0")
            .with("bottom", "0")
            .height("var(--workbench-border-width)")
            .with("transform-origin", "left")
            .background("primary")
            .with(
                "animation",
                "roost-countdown-drain var(--roost-undo-window-ms, 5000ms) linear forwards",
            )
            .media(
                "(prefers-reduced-motion: reduce)",
                sx().with("animation", "none"),
            )
    });
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
        Paper {
            shadow: "md", radius: "md", sx: card_sx,
            "data-testid": "undo-close-banner",
            "data-session-id": entry.session_id.clone(),
            Flex { direction: "column", gap: "xs", sx: label_sx,
                Text { size: "sm", "data-testid": "undo-snackbar-text", sx: truncate_sx.clone(),
                    strong { "{entry.labels.terminal_name}" }
                    " closed"
                }
                if !sub.is_empty() {
                    Text { size: "xs", color: "muted", sx: truncate_sx,
                        "data-testid": "undo-snackbar-sub", "{sub}"
                    }
                }
            }
            Button { variant: "standard", size: "sm",
                "data-testid": "undo-snackbar-action", onclick: undo, "Undo"
            }
            Box { aria_hidden: "true", sx: countdown_sx }
        }
    }
}
