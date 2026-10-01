//! The requester's one card: the primary request action, the token-bound poll
//! status, and the six-digit verification entry.
//!
//! Ported from `apps/web/src/components/pairing/OnboardingRequestCard.tsx`.
//! The opaque request id is never rendered — it is a capability for the tab
//! holding it, and a page that printed it would have printed half the pair.

use dioxus::prelude::*;
use roost_client_core::client::auth::PairPollStatus;

use crate::components::md::stylesheet::use_md_stylesheet;
use crate::components::md::{Button, ButtonVariant, Card, CardVariant, StatusDot, TextField};
use crate::pump::use_store;

use super::notices::ONBOARDING_STYLESHEET_HREF;
use super::requester::{PairingRequester, RequesterState};
use super::status::{
    poll_indicator, poll_message, request_restart_label, request_start_label, shows_request_actions,
};

/// The requester's card and every action on it.
#[component]
pub fn RequestAccessCard(requester: PairingRequester) -> Element {
    use_md_stylesheet(ONBOARDING_STYLESHEET_HREF);
    // The pump's revision is read so a store move repaints this card: the
    // authorization that ends a ceremony arrives as a store mutation, not as an
    // answer to any call this card made.
    let _pump = use_store();
    let state = requester.state().read().clone();
    let idle = state.status == PairPollStatus::Idle;
    rsx! {
        Card {
            title: "Request access",
            variant: CardVariant::Outlined,
            test_id: "onboarding-pair-step",
            div { class: "pairing-request",
                if idle {
                    IdlePrompt { requester: requester.clone(), state: state.clone() }
                } else {
                    RequestStatus { state: state.clone() }
                    if state.status == PairPollStatus::VerificationRequired {
                        VerificationEntry { requester: requester.clone(), state: state.clone() }
                    }
                    if shows_request_actions(&state.status) {
                        RequestActions { requester: requester.clone(), state: state.clone() }
                    }
                }
            }
        }
    }
}

/// What a browser with no request in flight is shown, and the one primary
/// action that starts one.
#[component]
fn IdlePrompt(requester: PairingRequester, state: RequesterState) -> Element {
    rsx! {
        p { class: "md-body-m pairing-request__supporting",
            "A browser that is already paired approves the request, then shows a 6-digit code for you to enter here."
        }
        div {
            Button {
                "data-testid": "onboarding-pair-start-btn",
                disabled: state.busy,
                onclick: move |_| requester.start(),
                "{request_start_label(state.busy, state.failed)}"
            }
        }
    }
}

/// The status line, and nothing at all when the page's own notice already
/// reports the failure — two indicators for one failure is one too many.
#[component]
fn RequestStatus(state: RequesterState) -> Element {
    if state.request_failure.is_some() {
        return rsx! {};
    }
    let indicator = poll_indicator(&state.status, state.failure.as_deref(), state.failed);
    let Some(message) = poll_message(&state.status, state.failure.as_deref(), state.failed) else {
        return rsx! {};
    };
    rsx! {
        div {
            class: "pairing-request__status",
            "data-testid": "onboarding-pair-poll-status",
            role: "status",
            "aria-live": "polite",
            "aria-atomic": "true",
            StatusDot { status: indicator.to_string() }
            span { class: "md-body-m", {message} }
        }
    }
}

/// The six-digit field, shown only once an approver has bound a code.
#[component]
fn VerificationEntry(requester: PairingRequester, state: RequesterState) -> Element {
    // Each handler is built once: an `rsx!` attribute body is a block, and a
    // block that clones per attribute moves the same `requester` once per
    // attribute it appears in.
    let on_input = {
        let requester = requester.clone();
        move |value: String| requester.update_verification_code(value)
    };
    let on_key_down = {
        let requester = requester.clone();
        move |event: KeyboardEvent| {
            if event.key() == Key::Enter {
                event.prevent_default();
                requester.confirm();
            }
        }
    };
    rsx! {
        TextField {
            value: state.verification_code.clone(),
            on_input,
            label: Some("Verification code".to_string()),
            placeholder: Some("123 456".to_string()),
            test_id: Some("onboarding-pair-verification-input".to_string()),
            autocomplete: Some("one-time-code".to_string()),
            input_mode: Some("numeric".to_string()),
            max_length: Some(7),
            aria_invalid: state.failure.is_some(),
            error: state.failure.clone().map(|message| rsx! { span { {message} } }),
            onkeydown: on_key_down,
        }
    }
}

/// The two actions a live request ends in: prove the code, or start again.
#[component]
fn RequestActions(requester: PairingRequester, state: RequesterState) -> Element {
    let on_confirm = {
        let requester = requester.clone();
        move |_event: MouseEvent| requester.confirm()
    };
    let on_start = {
        let requester = requester.clone();
        move |_event: MouseEvent| requester.start()
    };
    rsx! {
        div { class: "pairing-request__actions",
            if state.status == PairPollStatus::VerificationRequired {
                Button {
                    "data-testid": "onboarding-pair-confirm",
                    disabled: state.busy || state.verification_code.trim().is_empty(),
                    onclick: on_confirm,
                    if state.busy { "Verifying…" } else { "Verify browser" }
                }
            }
            Button {
                variant: ButtonVariant::Secondary,
                disabled: state.busy,
                onclick: on_start,
                "{request_restart_label(&state.status)}"
            }
        }
    }
}
