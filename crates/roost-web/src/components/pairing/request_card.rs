//! The requester's one card: the primary request action, the token-bound poll
//! status, and the six-digit verification entry. The opaque request id is never
//! rendered: it is a capability for the tab holding it, and a page that printed
//! it would have printed half the pair.

use dioxus::prelude::*;
use roost_client_core::client::auth::PairPollStatus;

use crate::components::ui::field::scoped_element_id;
use crate::components::ui::{
    Button, ButtonVariant, Card, CardContent, CardHeader, CardTitle, CardVariant, FieldError,
    FieldLabel, StatusDot,
};
use crate::pump::use_store;

use super::requester::{PairingRequester, RequesterState};
use super::status::{
    poll_indicator, poll_message, request_restart_label, request_start_label, shows_request_actions,
};

/// The requester's card and every action on it.
#[component]
pub fn RequestAccessCard(requester: PairingRequester) -> Element {
    // The pump's revision is read so a store move repaints this card: the
    // authorization that ends a ceremony arrives as a store mutation, not as an
    // answer to any call this card made.
    let _pump = use_store();
    let state = requester.state().read().clone();
    let idle = state.status == PairPollStatus::Idle;
    rsx! {
        Card { variant: CardVariant::Outlined, "data-testid": "onboarding-pair-step",
            CardHeader {
                CardTitle { "Request access" }
            }
            CardContent {
                div { class: "flex min-w-0 flex-col gap-3",
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
}

/// What a browser with no request in flight is shown, and the one primary
/// action that starts one.
#[component]
fn IdlePrompt(requester: PairingRequester, state: RequesterState) -> Element {
    rsx! {
        p { class: "m-0 text-sm text-muted-foreground",
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
            class: "flex items-center gap-2",
            "data-testid": "onboarding-pair-poll-status",
            role: "status",
            "aria-live": "polite",
            "aria-atomic": "true",
            StatusDot { status: indicator.to_string() }
            span { class: "text-sm", {message} }
        }
    }
}

/// Digits in a verification code. The coordinator's code is always this long.
const VERIFICATION_CODE_DIGITS: usize = 6;

/// A cell the caret is not in: filled, or still empty.
const OTP_CELL_IDLE: &str = "flex h-12 w-10 items-center justify-center rounded-md border border-input bg-background text-lg font-semibold text-foreground shadow-xs";

/// The cell the next digit lands in. The native caret is hidden under the
/// cells, so the ring is what shows where typing goes.
const OTP_CELL_ACTIVE: &str = "flex h-12 w-10 items-center justify-center rounded-md border border-ring bg-background text-lg font-semibold text-foreground shadow-xs ring-3 ring-ring/50";

/// The digits a verification entry keeps, capped at the code length. Letters and
/// separators a reader types or pastes never reach the coordinator, and the
/// confirm path compacts spaces anyway, so only digits can change what is sent.
fn verification_digits(typed: &str) -> String {
    typed
        .chars()
        .filter(char::is_ascii_digit)
        .take(VERIFICATION_CODE_DIGITS)
        .collect()
}

/// The six-digit field, shown only once an approver has bound a code.
///
/// One real input carries the value, the keyboard, paste and autofill. Six cells
/// draw it, and the input is transparent over the group, so a click on any cell
/// focuses the input and every native input behaviour stays as it was.
#[component]
fn VerificationEntry(requester: PairingRequester, state: RequesterState) -> Element {
    let typed = verification_digits(&state.verification_code);
    let digits: Vec<char> = typed.chars().collect();
    let active_index = digits.len().min(VERIFICATION_CODE_DIGITS - 1);
    let error_id = use_hook(|| scoped_element_id("onboarding-verification-error"));
    let failure = state.failure.clone();
    let invalid = if failure.is_some() { "true" } else { "false" };
    let described_by = failure.is_some().then(|| error_id.clone());
    let input_requester = requester.clone();
    let confirm_requester = requester;
    rsx! {
        div { class: "flex flex-col gap-2",
            FieldLabel { "Verification code" }
            div {
                role: "group",
                "aria-label": "Verification code",
                class: "relative flex w-fit gap-2",
                onkeydown: move |event: KeyboardEvent| {
                    if event.key() == Key::Enter {
                        event.prevent_default();
                        confirm_requester.confirm();
                    }
                },
                for index in 0..VERIFICATION_CODE_DIGITS {
                    span {
                        class: if index == active_index { OTP_CELL_ACTIVE } else { OTP_CELL_IDLE },
                        {digits.get(index).map(char::to_string).unwrap_or_default()}
                    }
                }
                input {
                    class: "absolute inset-0 size-full cursor-text opacity-0",
                    "aria-label": "Verification code",
                    "data-testid": "onboarding-pair-verification-input",
                    "aria-invalid": invalid,
                    "aria-describedby": described_by,
                    value: "{typed}",
                    maxlength: "{VERIFICATION_CODE_DIGITS}",
                    autocomplete: "one-time-code",
                    inputmode: "numeric",
                    oninput: move |event: FormEvent| {
                        input_requester.update_verification_code(verification_digits(&event.value()));
                    },
                }
            }
            if let Some(message) = failure {
                FieldError { id: error_id, {message} }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::verification_digits;

    #[test]
    fn the_entry_keeps_six_digits_and_drops_separators_and_letters() {
        assert_eq!(verification_digits("123 456"), "123456");
        assert_eq!(verification_digits("12a34-5678"), "123456");
        assert_eq!(verification_digits("12"), "12");
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
        div { class: "flex flex-wrap gap-2",
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
