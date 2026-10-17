//! The one surface that aggregates every upload and download. It is a dock
//! child, so jobs survive pane switches without claiming a corner of their
//! own; each per-file progress row reads the shared transfer ledger.
//! Upload previews are released when their keyed row leaves the list.
//! Ports `apps/web/src/components/notifications/TransferCard.tsx`.

use dioxus::prelude::*;

use super::transfer_row::TransferRow;
use crate::components::ui::{List, Surface, SurfaceElement, SurfaceRadius};
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
        Surface {
            element: SurfaceElement::Section,
            test_id: Some("transfer-card".to_owned()),
            aria_labelledby: Some("transfer-popup-title".to_owned()),
            level: 2,
            elevation: 3,
            radius: SurfaceRadius::Md,
            border: true,
            class: "flex flex-col gap-1 pt-2",
            div {
                class: "flex items-center gap-2 px-4",
                span {
                    id: "transfer-popup-title",
                    class: "flex-1 text-sm font-medium",
                    "Transfers"
                }
                span {
                    class: "text-xs text-muted-foreground",
                    "{count}"
                }
            }
            List {
                for card in cards {
                    TransferRow { key: "{card.id}", transfer: card }
                }
            }
        }
    }
}
