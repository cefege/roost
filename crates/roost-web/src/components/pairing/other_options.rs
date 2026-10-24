//! The collapsed "Other pairing options": manual redemption of a one-time setup
//! token, and creating a new device key for a browser the coordinator revoked.
//! The redeem is the road the `#pair=` fragment takes at boot, because a token an
//! operator hands over as text has to be redeemable from the page too.

use dioxus::prelude::*;
use roost_client_core::client::auth::redeem::AuthRedeemBrowserRequest;
use roost_client_core::client::rpc::calls::pairing::RedeemPairToken;

use libero::{
    components::{Button, Flex, Paper, Pictogram, Text, TextField, Title},
    sx::sx,
};

use crate::platform::device_key::WebDeviceKey;
use crate::platform::self_label::current_browser_self_label;
use crate::pump::{Pump, use_pump};

use super::failure::describe;
use super::notices::{NoticeTone, PairingStatusNotice};
use super::requester::PairingRequester;

/// The prefix every setup token carries, which is also what makes a paste
/// recognisable as a token rather than as whatever else was on the clipboard.
const SETUP_TOKEN_PREFIX: &str = "roost_bt_";

/// The disclosure, and the two roads out of the gate that need no approver.
#[component]
pub fn PairingOtherOptions(requester: PairingRequester) -> Element {
    let pump: Pump = use_pump();
    let mut open = use_signal(|| false);
    let mut setup_token = use_signal(String::new);
    let mut redeeming = use_signal(|| false);
    let mut redeem_error = use_signal(|| None::<String>);
    let mut paired = use_signal(|| false);
    let mut resetting = use_signal(|| false);
    let mut reset_error = use_signal(|| None::<String>);
    let panel_id = libero::hooks::use_id().read().clone();

    let redeem = use_callback({
        let pump = pump.clone();
        let requester = requester.clone();
        move |token: String| {
            let pump = pump.clone();
            let requester = requester.clone();
            spawn(async move {
                redeeming.set(true);
                redeem_error.set(None);
                match redeem_setup_token(&pump, &token).await {
                    Ok(()) => {
                        // A requester ceremony left behind would restart its poll
                        // after the redirect, on a browser that is already paired.
                        requester.clear();
                        paired.set(true);
                        crate::platform::location::replace_location("/");
                    }
                    Err(message) => {
                        redeeming.set(false);
                        redeem_error.set(Some(message));
                    }
                }
            });
        }
    });

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
    let token_value = setup_token.read().clone();
    let reset_busy = *resetting.read();
    let reset_message = reset_error.read().clone();
    let card_sx = use_hook(|| sx().padding("md").width("100%"));
    rsx! {
        Paper { sx: card_sx, bordered: true,
            Flex { direction: "column", gap: "md",
                Flex { direction: "row", justify: "space-between", align: "center",
                    Title { size: "md", "Other pairing options" }
                    Button {
                        variant: "text",
                        icon: rsx! { Pictogram { icon: if expanded { pictogram_icons_lucide::chevron_up::outlined } else { pictogram_icons_lucide::chevron_down::outlined }, width: "16", height: "16" } },
                        "aria-expanded": expanded.to_string(),
                        "aria-controls": panel_id.clone(),
                        "data-testid": "pairing-other-options-toggle",
                        onclick: move |_| open.toggle(),
                        if expanded { "Hide" } else { "Show" }
                    }
                }
                if expanded {
                    Flex { direction: "column", gap: "lg",
                        id: panel_id,
                        "data-testid": "pairing-other-options-panel",
                        SetupTokenSection {
                            token: token_value.clone(),
                            redeeming: *redeeming.read(),
                            redeem_error: redeem_error.read().clone(),
                            paired: *paired.read(),
                            on_token: move |value: String| setup_token.set(value),
                            on_redeem: move |_| redeem.call(token_value.trim().to_string()),
                        }
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

/// The setup-token field and the one button that spends it.
#[component]
fn SetupTokenSection(
    token: String,
    redeeming: bool,
    redeem_error: Option<String>,
    paired: bool,
    on_token: EventHandler<String>,
    on_redeem: EventHandler<()>,
) -> Element {
    rsx! {
        Flex { direction: "column", gap: "sm",
            Title { size: "sm", component: "h3", "Use a one-time setup token" }
            TextField {
                value: token.clone(),
                oninput: move |value: String| on_token.call(value),
                label: "Setup token",
                placeholder: format!("{SETUP_TOKEN_PREFIX}…"),
                "data-testid": "onboarding-setup-token-input",
                autocomplete: "off",
            }
            Button {
                variant: "tonal",
                "data-testid": "onboarding-setup-token-submit",
                disabled: token.trim().is_empty() || redeeming,
                onclick: move |_| on_redeem.call(()),
                if redeeming { "Pairing…" } else { "Pair with token" }
            }
            if let Some(message) = redeem_error {
                PairingStatusNotice { tone: NoticeTone::Error, message: format!("Redeem failed: {message}"), test_id: None }
            }
            if paired {
                PairingStatusNotice { tone: NoticeTone::Ok, message: "Browser paired. Opening Roost…".to_owned(), test_id: None }
            }
        }
    }
}

/// The revoked-key escape hatch.
#[component]
fn KeyRecoverySection(busy: bool, error: Option<String>, on_reset: EventHandler<()>) -> Element {
    rsx! {
        Flex { direction: "column", gap: "sm", "data-testid": "pairing-key-recovery",
            Title { size: "sm", component: "h3", "Recover a rejected browser key" }
            Text { size: "sm", color: "muted",
                "If this browser was previously paired and revoked, create a new local device key before requesting access again."
            }
            Button {
                variant: "tonal",
                "data-testid": "onboarding-reset-key-btn",
                disabled: busy,
                onclick: move |_| on_reset.call(()),
                if busy { "Creating…" } else { "Create new device key" }
            }
            if let Some(message) = error {
                PairingStatusNotice { tone: NoticeTone::Error, message, test_id: None }
            }
        }
    }
}

/// Spend `token` on this browser's key.
///
/// The transport's own words are returned rather than a rewritten summary, so a
/// `FailedPrecondition: pairing client must reload` reads as the instruction it
/// is (`redeemPairToken.ts:42-52`).
async fn redeem_setup_token(pump: &Pump, token: &str) -> Result<(), String> {
    if !token.starts_with(SETUP_TOKEN_PREFIX) {
        return Err("That is not a Roost setup token.".to_string());
    }
    let ssh_pubkey_b64 = match pump.rpc().device_key() {
        Some(key) => key.public_key_b64().to_owned(),
        None => WebDeviceKey::load_or_generate()
            .await
            .map_err(|reason| format!("This browser has no device key: {reason}"))?
            .public_key_b64()
            .to_owned(),
    };
    let request = AuthRedeemBrowserRequest {
        token: token.to_string(),
        ssh_pubkey_b64,
        label: current_browser_self_label(),
    };
    match pump.rpc().call_public(&RedeemPairToken { request }).await {
        Ok(()) => {
            tracing::info!(target: "auth", "setup token redeemed");
            Ok(())
        }
        Err(error) => Err(describe(&error)),
    }
}
