//! The one surface that aggregates every upload and download. It is a dock
//! child, so jobs survive pane switches without claiming a corner of their
//! own; each per-file progress row reads the shared transfer ledger.
//! Ports `apps/web/src/components/notifications/TransferCard.tsx`.

use dioxus::prelude::*;

use super::inline_transfer::use_inline_transfer_presentation;
use super::transfer_row::TransferRow;
use crate::components::ui::{Card, CardAction, CardContent, CardHeader, CardTitle, List};
use crate::pump::use_store;

/// The popup, or nothing when no transfer needs separate dock presentation.
#[component]
pub fn TransfersPanel() -> Element {
    let pump = use_store();
    let inline_presentation = use_inline_transfer_presentation();
    let cards = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .transfers
            .transfers()
            .filter(|transfer| {
                !inline_presentation.is_some_and(|presentation| {
                    presentation.suppresses(&transfer.id, transfer.state)
                })
            })
            .cloned()
            .collect::<Vec<_>>()
    };
    if cards.is_empty() {
        return rsx! {};
    }
    let count = cards.len();
    rsx! {
        Card {
            "data-testid": "transfer-card",
            role: "region",
            "aria-labelledby": "transfer-popup-title",
            CardHeader {
                CardTitle { id: "transfer-popup-title", "Transfers" }
                CardAction { span { "{count}" } }
            }
            CardContent {
                List {
                    for card in cards {
                        TransferRow { key: "{card.id}", transfer: card }
                    }
                }
            }
        }
    }
}
