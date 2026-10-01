//! The dock's live toast cards, in the order their identities were first
//! raised. The stack itself is `roost_client_core::store::toasts::ToastStack`,
//! whose identity rule already makes "the same event produced two cards"
//! unrepresentable; this file only walks it.
//! Ports the whole of `apps/web/src/components/notifications/ToastStack.tsx`.

use dioxus::prelude::*;

use super::toast_card::ToastCard;
use crate::pump::use_store;

/// Every live card, oldest first.
#[component]
pub fn ToastStack() -> Element {
    let pump = use_store();
    let ids = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .toasts
            .toasts()
            .map(|toast| toast.id.clone())
            .collect::<Vec<_>>()
    };
    if ids.is_empty() {
        return rsx! {};
    }
    rsx! {
        div {
            role: "region",
            "aria-label": "Notifications",
            style: "display: flex; flex-direction: column; align-items: center; gap: var(--md-space-2);",
            for id in ids {
                ToastCard { toast_id: id }
            }
        }
    }
}
