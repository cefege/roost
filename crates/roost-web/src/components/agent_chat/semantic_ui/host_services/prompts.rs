//! Presentational prompts for OMP host-service requests.
//!
//! The cards are stock rust-ui surfaces and carry no state of their own: every
//! button calls the root-scoped controller, which survives a dialog remount.

use dioxus::prelude::*;
use roost_client_core::client::agent_chat::OmpHostServiceKind;

use crate::components::ui::{Button, ButtonSize, ButtonVariant, Card, CardSize};

use super::controller::OmpHostServices;

/// The payload came from the worker; only the clipboard step is browser-local.
pub(super) const COPY_DETAIL: &str = "OMP produced this text on the conversation's worker; this browser handles the clipboard, and the text is not saved in this conversation's history.";

#[component]
pub(crate) fn OmpHostServicePrompts() -> Element {
    let Some(services) = try_use_context::<OmpHostServices>() else {
        return rsx! {};
    };
    let requests = services.requests();
    if requests.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "flex shrink-0 flex-col gap-2 px-4 py-2",
            for service in requests {
                {
                    let service_id = service.service_id.clone();
                    let failure = services.failure(&service_id);
                    let busy = services.is_sending(&service_id);
                    let is_copy = service.kind == OmpHostServiceKind::CopyText;
                    let is_stats = service.kind == OmpHostServiceKind::OpenStatsDashboard;
                    let (label, detail) = if is_copy {
                        (
                            "OMP wants to copy text to your clipboard.",
                            COPY_DETAIL,
                        )
                    } else if is_stats {
                        (
                            "OMP wants to open the stats dashboard from this conversation's worker.",
                            "The dashboard runs on the worker and opens in a separate tab without access to this app's storage or your session.",
                        )
                    } else {
                        (
                            "OMP wants to open a link in this browser.",
                            "Only http(s) links and files on the conversation's worker can open.",
                        )
                    };
                    let copy_service = service.clone();
                    let open_service = service.clone();
                    let stats_service = service.clone();
                    let dismiss_service = service.clone();
                    rsx! {
                        Card { size: CardSize::Sm, class: Some("flex flex-wrap items-center gap-3 p-3".into()),
                            div { class: "min-w-0 flex-1",
                                p { class: "text-sm", "{label}" }
                                p { class: "text-xs text-muted-foreground", "{detail}" }
                                if let Some(failure) = failure {
                                    p { role: "alert", class: "text-xs text-destructive", "{failure}" }
                                }
                            }
                            if is_copy {
                                Button {
                                    size: ButtonSize::Sm,
                                    variant: ButtonVariant::Secondary,
                                    disabled: busy,
                                    onclick: move |_| services.copy(copy_service.clone()),
                                    if busy { "Copying…" } else { "Copy" }
                                }
                            } else if is_stats {
                                Button {
                                    size: ButtonSize::Sm,
                                    variant: ButtonVariant::Secondary,
                                    disabled: busy,
                                    onclick: move |_| services.open_stats(stats_service.clone()),
                                    if busy { "Opening…" } else { "Open dashboard" }
                                }
                            } else {
                                Button {
                                    size: ButtonSize::Sm,
                                    variant: ButtonVariant::Secondary,
                                    disabled: busy,
                                    onclick: move |_| services.open(open_service.clone()),
                                    if busy { "Opening…" } else { "Open" }
                                }
                            }
                            Button {
                                size: ButtonSize::Sm,
                                variant: ButtonVariant::Ghost,
                                onclick: move |_| services.dismiss(dismiss_service.clone()),
                                "Dismiss"
                            }
                        }
                    }
                }
            }
        }
    }
}
