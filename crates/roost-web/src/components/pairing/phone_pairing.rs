//! "Pair a phone": a paired browser mints a one-shot browser grant and shows
//! its `#pair=` link as a QR code, so a phone camera opens Roost and pairs with
//! no typing.
//!
//! Mounted by Settings → Devices and by the approver half of the pairing page.
//! Depends on `settings::bootstrap::MintBootstrap` for the grant,
//! `machines::enrollment_origin` for the origin a phone can reach,
//! `roost_platform::browser_pairing_link` for the link the boot captures, and
//! `pairing::phone_qr` for the drawing.
//!
//! NOTHING IS MINTED UNTIL ASKED. A grant is a live credential for 24 hours, so
//! opening the pane mints none; one press mints exactly one, and the press is
//! disabled while it is in flight. The redemption, its one-shot claim and its
//! expiry are the coordinator's, unchanged: this card only carries the grant.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::bootstrap::{BootstrapKind, MintBootstrap};
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::machines::GetCoordinatorIdentity;

#[cfg(target_arch = "wasm32")]
use super::phone_qr::pairing_qr_svg;
#[cfg(target_arch = "wasm32")]
use crate::components::machines::enrollment_origin::{EnrollmentDecision, enrollment_decision};
use crate::components::md::{Button, ButtonVariant, Card};
use crate::components::notifications::clipboard;
use crate::components::terminal::dom::{now_ms, sleep_ms};
use crate::pump::{Pump, use_store};

/// The label the coordinator records against a grant this card mints, shown in
/// the device list's provenance until the phone spends it.
#[cfg(target_arch = "wasm32")]
const PHONE_GRANT_LABEL: &str = "phone-qr";

/// How often the expiry line re-reads the clock. A minute is the line's own
/// resolution, so a faster tick would repaint the same words.
const EXPIRY_TICK_MS: u64 = 30_000;

/// A minted grant, ready to scan.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScannablePairing {
    link: String,
    qr_svg: String,
    expires_at_ms: u64,
}

/// Where the card is. Every state past `Minting` is an answer from the
/// coordinator, which only a browser build asks for.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
enum PhonePairing {
    #[default]
    Idle,
    Minting,
    Ready(ScannablePairing),
    /// No origin a phone can open, which is a deployment fact rather than a
    /// failure: the sentence says what to declare.
    Unreachable(String),
    Failed(String),
}

/// The card.
#[component]
pub fn PhonePairingCard() -> Element {
    let pump = use_store();
    let state = use_signal(PhonePairing::default);
    let mut clock = use_signal(now_ms);
    use_future(move || async move {
        loop {
            sleep_ms(EXPIRY_TICK_MS).await;
            clock.set(now_ms());
        }
    });
    let current = state();
    let mint = move |_| mint_phone_grant(pump.clone(), state);
    rsx! {
        Card {
            title: "Pair a phone",
            supporting: "Scan this code with the phone's camera. Roost opens and pairs it — no typing.",
            test_id: "phone-pairing-card",
            match current {
                PhonePairing::Idle => rsx! {
                    Button {
                        variant: ButtonVariant::Default,
                        icon: "qr_code_2",
                        "data-testid": "phone-pairing-show",
                        onclick: mint,
                        "Show pairing code"
                    }
                },
                PhonePairing::Minting => rsx! {
                    Button { variant: ButtonVariant::Default, icon: "qr_code_2", disabled: true, "Generating…" }
                },
                PhonePairing::Ready(pairing) => rsx! {
                    ScannableCode { pairing, clock_ms: clock(), on_regenerate: mint }
                },
                PhonePairing::Unreachable(message) => rsx! {
                    p { class: "md-body-m", "data-testid": "phone-pairing-unreachable", style: "color: var(--md-sys-color-on-surface-variant); margin: 0;",
                        {message}
                    }
                    Button { variant: ButtonVariant::Secondary, onclick: mint, "Check again" }
                },
                PhonePairing::Failed(message) => rsx! {
                    p { role: "alert", class: "md-body-m", style: "color: var(--md-sys-color-error); margin: 0;",
                        {message}
                    }
                    Button { variant: ButtonVariant::Secondary, onclick: mint, "Try again" }
                },
            }
        }
    }
}

/// The code, its expiry, and the two actions beside it. An expired grant draws
/// no code at all: a code that scans and then refuses teaches the reader that
/// scanning does not work.
#[component]
fn ScannableCode(
    pairing: ScannablePairing,
    clock_ms: u64,
    on_regenerate: EventHandler<MouseEvent>,
) -> Element {
    let mut copied = use_signal(|| false);
    let remaining = expiry_phrase(clock_ms, pairing.expires_at_ms);
    let link = pairing.link.clone();
    rsx! {
        div { style: "display: flex; flex-wrap: wrap; align-items: center; gap: var(--md-space-5);",
            if let Some(remaining) = remaining.clone() {
                div {
                    role: "img",
                    "aria-label": "QR code that pairs a phone with this Roost",
                    "data-testid": "phone-pairing-qr",
                    style: "color: var(--qr-module); background: var(--qr-field); border-radius: var(--md-shape-sm); line-height: 0; flex-shrink: 0;",
                    title: remaining,
                    dangerous_inner_html: pairing.qr_svg.clone(),
                }
            }
            div { style: "display: flex; flex-direction: column; gap: var(--md-space-3); min-width: 0;",
                p { class: "md-body-m", "data-testid": "phone-pairing-expiry", style: "margin: 0;",
                    {remaining.unwrap_or_else(|| "This code has expired.".to_owned())}
                }
                p { class: "md-body-s", style: "color: var(--md-sys-color-on-surface-variant); margin: 0;",
                    "It works once: whoever opens it first pairs, so show it only to the phone you mean."
                }
                div { style: "display: flex; flex-wrap: wrap; gap: var(--md-space-2);",
                    Button {
                        variant: ButtonVariant::Secondary,
                        icon: "refresh",
                        "data-testid": "phone-pairing-regenerate",
                        onclick: move |event| {
                            copied.set(false);
                            on_regenerate.call(event);
                        },
                        "New code"
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        icon: "content_copy",
                        onclick: move |_| copied.set(clipboard::copy_text(&link)),
                        if copied() { "Link copied" } else { "Copy link" }
                    }
                }
            }
        }
    }
}

/// How long a grant has left, in the words the card shows, or `None` once it
/// has expired.
fn expiry_phrase(now_ms: u64, expires_at_ms: u64) -> Option<String> {
    let remaining_ms = expires_at_ms.checked_sub(now_ms).filter(|left| *left > 0)?;
    let minutes = remaining_ms / 60_000;
    Some(match (minutes / 60, minutes % 60) {
        (0, 0) => "Expires in under a minute.".to_owned(),
        (0, minutes) => format!("Expires in {minutes} min."),
        (hours, 0) => format!("Expires in {hours} h."),
        (hours, minutes) => format!("Expires in {hours} h {minutes} min."),
    })
}

/// Decide the origin, mint one browser grant, and draw its link.
///
/// The origin is re-read on every press for the reason the deploy dialog gives:
/// the declared front door may have changed since the pane opened, and a code
/// pointing at a door the phone cannot open is a grant spent on nothing.
fn mint_phone_grant(pump: Pump, mut state: Signal<PhonePairing>) {
    if *state.peek() == PhonePairing::Minting {
        return;
    }
    state.set(PhonePairing::Minting);
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let next = mint_scannable_pairing(&pump).await;
        state.set(next);
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = pump;
}

#[cfg(target_arch = "wasm32")]
async fn mint_scannable_pairing(pump: &Pump) -> PhonePairing {
    let rpc = pump.rpc();
    let declared = match rpc.call(&GetCoordinatorIdentity).await {
        Ok(declared) => declared,
        Err(error) => {
            tracing::warn!(target: "pairing", %error, "phone pairing origin check refused");
            return PhonePairing::Failed(format!("The coordinator did not answer: {error}"));
        }
    };
    let origin = match enrollment_decision(&declared, rpc.base_url()) {
        EnrollmentDecision::Ready { coordinator_url } => coordinator_url,
        EnrollmentDecision::LocalOnly => {
            return PhonePairing::Unreachable(
                "This Roost is reachable only from this computer, so a phone has nothing to \
                 open. Declare an HTTPS front door (ROOST_WEB_PUBLIC_URL) to pair a phone by \
                 scanning."
                    .to_owned(),
            );
        }
        EnrollmentDecision::ConfigurationError { declared_url } => {
            return PhonePairing::Unreachable(format!(
                "The coordinator declares {declared_url}, which a phone cannot open over HTTPS. \
                 Fix the declared address to pair a phone by scanning."
            ));
        }
    };
    let request = MintBootstrap {
        kind: BootstrapKind::Browser,
        label: PHONE_GRANT_LABEL.to_owned(),
    };
    let grant = match rpc.call(&request).await {
        Ok(grant) => grant,
        Err(error) => {
            tracing::warn!(target: "pairing", %error, "phone pairing grant refused");
            return PhonePairing::Failed(format!("No pairing code was issued: {error}"));
        }
    };
    let link = roost_platform::browser_pairing_link(&origin, &grant.token);
    match pairing_qr_svg(&link) {
        Ok(qr_svg) => {
            tracing::info!(target: "pairing", origin = %origin, expires_at_ms = grant.expires_at_ms, "phone pairing code issued");
            PhonePairing::Ready(ScannablePairing {
                link,
                qr_svg,
                expires_at_ms: grant.expires_at_ms,
            })
        }
        Err(error) => {
            tracing::error!(target: "pairing", %error, "phone pairing link could not be encoded");
            PhonePairing::Failed(format!("The pairing link could not be drawn: {error}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::expiry_phrase;

    const MINUTE_MS: u64 = 60_000;

    #[test]
    fn the_expiry_line_counts_down_in_whole_minutes_and_then_stops() {
        assert_eq!(
            expiry_phrase(0, 24 * 60 * MINUTE_MS).as_deref(),
            Some("Expires in 24 h.")
        );
        assert_eq!(
            expiry_phrase(0, 90 * MINUTE_MS + 59_999).as_deref(),
            Some("Expires in 1 h 30 min.")
        );
        assert_eq!(
            expiry_phrase(0, 5 * MINUTE_MS).as_deref(),
            Some("Expires in 5 min.")
        );
        assert_eq!(
            expiry_phrase(0, 59_000).as_deref(),
            Some("Expires in under a minute.")
        );
        assert_eq!(expiry_phrase(1_000, 1_000), None);
        assert_eq!(expiry_phrase(2_000, 1_000), None);
    }
}
