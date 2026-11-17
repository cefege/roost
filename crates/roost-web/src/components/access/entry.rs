//! One access entry for an origin, a fragment pairing link or a one-shot token.
//! Pairing still uses the existing public redeem method and document device key.
//! Links cross origins by navigation; credentials never enter saved bookmarks.

use dioxus::prelude::*;
use roost_client_core::client::access::enrollment::browser_origin;
use roost_client_core::client::auth::redeem::AuthRedeemBrowserRequest;
use roost_client_core::client::rpc::calls::pairing::RedeemPairToken;

use crate::components::pairing::PairingRequester;
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardDescription, CardHeader, CardTitle, FieldError,
    TextField,
};
use crate::platform::device_key::WebDeviceKey;
use crate::platform::fragment_credential::{FragmentCredential, parse_fragment_credential};
use crate::platform::self_label::current_browser_self_label;
use crate::pump::{Pump, use_pump};

pub enum AccessInput {
    Address(String),
    PairingLink { origin: String, token: String },
    Token(String),
}

/// Parse links without accepting a credential in the query string.
pub fn parse_access_input(raw: &str) -> Option<AccessInput> {
    let text = raw.trim();
    if text.starts_with("roost_bt_") && !text.contains(char::is_whitespace) {
        return Some(AccessInput::Token(text.to_owned()));
    }
    if let Some((address, fragment)) = text.split_once('#') {
        let address = address.strip_suffix("/pair").unwrap_or(address);
        let origin = browser_origin(address)?;
        let FragmentCredential::Pair { token } = parse_fragment_credential(fragment) else {
            return None;
        };
        if !token.starts_with("roost_bt_") {
            return None;
        }
        return Some(AccessInput::PairingLink { origin, token });
    }
    browser_origin(text).map(AccessInput::Address)
}

#[component]
pub fn ConnectEntry(
    #[props(default)] requester: Option<PairingRequester>,
    on_save: EventHandler<String>,
) -> Element {
    let pump = use_pump();
    let mut value = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut failure = use_signal(|| None::<String>);
    let activate = use_callback(move |()| {
        if busy() {
            return;
        }
        let Some(input) = parse_access_input(&value()) else {
            failure.set(Some("Enter a Roost HTTPS address, a #pair= link, or a one-time roost_bt_ token. HTTP is allowed only on this computer.".to_owned()));
            return;
        };
        failure.set(None);
        let token = match input {
            AccessInput::Address(origin) => {
                if !crate::platform::location::open_roost(&origin, None) {
                    failure.set(Some("The browser could not open that address.".to_owned()));
                }
                return;
            }
            AccessInput::PairingLink { origin, token } => {
                if origin != pump.rpc().base_url() {
                    if !crate::platform::location::open_roost(&origin, Some(&token)) {
                        failure.set(Some(
                            "The browser could not open that pairing link.".to_owned(),
                        ));
                    }
                    return;
                }
                token
            }
            AccessInput::Token(token) => token,
        };
        busy.set(true);
        value.set(String::new());
        let pump = pump.clone();
        let requester = requester.clone();
        spawn(async move {
            match redeem_setup_token(&pump, &token).await {
                Ok(()) => {
                    if let Some(requester) = requester {
                        requester.clear();
                    }
                    crate::platform::location::replace_location("/");
                }
                Err(message) => {
                    busy.set(false);
                    failure.set(Some(message));
                }
            }
        });
    });
    let saveable = browser_origin(&value());
    rsx! {
        Card {
            CardHeader {
                CardTitle { "Connect to Roost" }
                CardDescription { "Paste an address, pairing link or one-time setup token. A phone can open a pairing QR code with its camera." }
            }
            CardContent { class: "flex flex-col gap-3",
                div { onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Named(NamedKey::Enter) { event.prevent_default(); activate.call(()); }
                },
                    TextField {
                        value: value(),
                        on_input: move |input| { value.set(input); failure.set(None); },
                        label: Some("Address or pairing link".to_owned()),
                        placeholder: Some("https://your-roost.ts.net".to_owned()),
                        disabled: busy(),
                        autocomplete: Some("off".to_owned()),
                        test_id: Some("onboarding-setup-token-input".to_owned()),
                    }
                }
                if let Some(message) = failure() { FieldError { {message} } }
                div { class: "flex flex-wrap gap-2",
                    Button {
                        variant: ButtonVariant::Default,
                        "data-testid": "onboarding-setup-token-submit",
                        disabled: busy() || value().trim().is_empty(),
                        onclick: move |_| activate.call(()),
                        if busy() { "Pairing…" } else { "Continue" }
                    }
                    if let Some(origin) = saveable {
                        Button { variant: ButtonVariant::Secondary,
                            onclick: move |_| { on_save.call(origin.clone()); value.set(String::new()); },
                            "Save address"
                        }
                    }
                }
                p { class: "m-0 text-sm text-muted-foreground",
                    "Opening another address may require pairing this browser there. Tailscale or Cloudflare sign-in does not replace device approval."
                }
            }
        }
    }
}

async fn redeem_setup_token(pump: &Pump, token: &str) -> Result<(), String> {
    let ssh_pubkey_b64 = match pump.rpc().device_key() {
        Some(key) => key.public_key_b64().to_owned(),
        None => WebDeviceKey::load_or_generate()
            .await
            .map_err(|reason| format!("This browser has no device key: {reason}"))?
            .public_key_b64()
            .to_owned(),
    };
    let request = AuthRedeemBrowserRequest {
        token: token.to_owned(),
        ssh_pubkey_b64,
        label: current_browser_self_label(),
    };
    pump.rpc()
        .call_public(&RedeemPairToken { request })
        .await
        .map_err(|error| error.to_string())?;
    tracing::info!(target: "auth", "setup token redeemed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{AccessInput, parse_access_input};

    #[test]
    fn pairing_links_retain_their_destination_and_never_become_bookmarks() {
        let Some(AccessInput::PairingLink { origin, token }) =
            parse_access_input("https://home.test/#pair=roost_bt_example")
        else {
            panic!("expected pairing link");
        };
        assert_eq!(origin, "https://home.test");
        assert_eq!(token, "roost_bt_example");
        for invalid in [
            "https://home.test/?pair=roost_bt_secret",
            "https://user:secret@home.test",
            "https://home.test/#pair=a&pair=b",
            "http://remote.test",
            "javascript:alert(1)",
        ] {
            assert!(parse_access_input(invalid).is_none());
        }
    }
}
