//! The one surface that aggregates every upload and download. It is a dock
//! child, so jobs survive pane switches without claiming a corner of their
//! own; each per-file progress row reads the shared transfer ledger.
//! Ports `apps/web/src/components/notifications/TransferCard.tsx`.

use dioxus::prelude::*;

use super::transfer_row::TransferRow;
use crate::components::ui::{Card, CardAction, CardContent, CardHeader, CardTitle, List};
use crate::pump::use_store;

/// The popup, or nothing when no transfer is live.
#[component]
pub fn TransfersPanel() -> Element {
    let pump = use_store();
    let cards = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .transfers
            .transfers()
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
