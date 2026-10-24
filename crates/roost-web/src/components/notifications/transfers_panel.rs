//! The one surface that aggregates every upload and download. It is a dock
//! child, so jobs survive pane switches without claiming a corner of their
//! own; each per-file progress row reads the shared transfer ledger.
//! Upload previews are released when their keyed row leaves the list.
//! Ports `apps/web/src/components/notifications/TransferCard.tsx`.

use dioxus::prelude::*;

use super::transfer_row::TransferRow;
use crate::pump::use_store;
use libero::{
    components::{Flex, Paper, Text, Title},
    sx::sx,
};

/// The popup, or nothing when no transfer is live.
#[component]
pub fn TransfersPanel() -> Element {
    let pump = use_store();
    let panel_sx = use_hook(|| sx().padding("xs"));
    let heading_sx = use_hook(|| sx().flex("1"));
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
        Paper { component: "section", "data-testid": "transfer-card",
            aria_labelledby: "transfer-popup-title", shadow: "md",
            radius: "md", bordered: true, sx: panel_sx,
            Flex { direction: "column", gap: "xs",
                Flex { direction: "row", align: "center", gap: "xs", wrap: false,
                    Title { component: "h2", size: "sm", id: "transfer-popup-title",
                        sx: heading_sx, "Transfers"
                    }
                    Text { size: "xs", color: "muted", "{count}" }
                }
                Flex { direction: "column",
                    for card in cards { TransferRow { key: "{card.id}", transfer: card } }
                }
            }
        }
    }
}
