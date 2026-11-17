//! Explicit recovery for a browser key refused by the coordinator.
//! Mounted only by the pairing page; it never resets a key automatically.
//! Address and token entry belong to the shared Access surface.

use dioxus::prelude::*;

use super::notices::{NoticeTone, PairingStatusNotice};
use crate::components::ui::field::scoped_element_id;
use crate::components::ui::{
    Button, ButtonSize, ButtonVariant, Card, CardAction, CardContent, CardHeader, CardTitle,
    IconType,
};
use crate::platform::device_key::WebDeviceKey;
#[component]
pub fn KeyRecoveryCard() -> Element {
    let mut open = use_signal(|| false);
    let mut resetting = use_signal(|| false);
    let mut reset_error = use_signal(|| None::<String>);
    let panel_id = use_hook(|| scoped_element_id("pairing-options"));

    let reset_key = use_callback(move |()| {
        spawn(async move {
            resetting.set(true);
            reset_error.set(None);
            match WebDeviceKey::reset().await {
                Ok(()) => {
                    // The key this document holds is gone. Only a fresh load
                    // mints a new one, and only a new one is a browser the
                    // coordinator has not already revoked.
                    tracing::info!(target: "auth", "device key reset; reloading as a new browser");
                    crate::platform::location::replace_location("/");
                }
                Err(reason) => {
                    resetting.set(false);
                    reset_error.set(Some(format!("Key reset failed: {reason}")));
                }
            }
        });
    });

    let expanded = *open.read();
    let reset_busy = *resetting.read();
    let reset_message = reset_error.read().clone();
    rsx! {
        Card {
            CardHeader {
                CardTitle { "Browser key recovery" }
                CardAction {
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        icon: if expanded { Some(IconType::ChevronUp) } else { Some(IconType::ChevronDown) },
                        "aria-expanded": expanded.to_string(),
                        "aria-controls": panel_id.clone(),
                        "data-testid": "pairing-other-options-toggle",
                        onclick: move |_| open.toggle(),
                        if expanded { "Hide" } else { "Show" }
                    }
                }
            }
            if expanded {
                CardContent {
                    div {
                        id: panel_id,
                        class: "flex min-w-0 flex-col gap-5",
                        "data-testid": "pairing-other-options-panel",
                        KeyRecoverySection {
                            busy: reset_busy,
                            error: reset_message,
                            on_reset: move |_| reset_key.call(()),
                        }
                    }
                }
            }
        }
    }
}

/// The revoked-key escape hatch.
#[component]
fn KeyRecoverySection(busy: bool, error: Option<String>, on_reset: EventHandler<()>) -> Element {
    rsx! {
        div { class: "flex min-w-0 flex-col gap-3", "data-testid": "pairing-key-recovery",
            span { class: "text-foreground",
                "Recover a rejected browser key"
            }
            p { class: "m-0 text-muted-foreground",
                "If this browser was previously paired and revoked, create a new local device key before requesting access again."
            }
            div {
                Button {
                    variant: ButtonVariant::Secondary,
                    "data-testid": "onboarding-reset-key-btn",
                    disabled: busy,
                    onclick: move |_| on_reset.call(()),
                    if busy { "Creating…" } else { "Create new device key" }
                }
            }
            if let Some(message) = error {
                PairingStatusNotice { tone: NoticeTone::Error, message, test_id: None }
            }
        }
    }
}
