//! The one place a generated verification code is ever shown.
//!
//! Ported from `apps/web/src/components/pairing/PairVerificationCodeDialog.tsx`.
//! It is presentational: the code, the lifecycle state, and every dismissal
//! routed to one cancel callback that the approver turns into a `PairDeny`.
//! There is no other close affordance, because a close that did not withdraw
//! the approval would leave a live six-digit code the coordinator still honours
//! with nobody holding the other half.

use dioxus::prelude::*;

use super::approver::{CodeDialogState, PairApprover};
use super::notices::{NoticeTone, PairingStatusNotice, STATUS_NOTICE_STYLESHEET_HREF};
use super::status::group_verification_code;
use crate::components::md::stylesheet::use_md_stylesheet;
use crate::components::md::{Button, ButtonVariant, Dialog, Surface, SurfaceRadius};

/// The dialog's own sheet, served from `assets/components/pairing/`.
pub const CODE_DIALOG_STYLESHEET_HREF: &str = "/components/pairing/PairVerificationCodeDialog.css";

/// The code, the state, and the two ways out of it.
#[component]
pub fn PairVerificationCodeDialog(
    verification_code: String,
    requester_label: String,
    state: CodeDialogState,
    on_cancel: EventHandler<()>,
    on_reload: EventHandler<()>,
) -> Element {
    use_md_stylesheet(CODE_DIALOG_STYLESHEET_HREF);
    use_md_stylesheet(STATUS_NOTICE_STYLESHEET_HREF);
    let reload_required = state == CodeDialogState::ReloadRequired;
    let cancelling = state == CodeDialogState::Cancelling;
    let subject = if requester_label.trim().is_empty() {
        "the requesting browser".to_string()
    } else {
        requester_label.clone()
    };
    let description = rsx! {
        span {
            "Enter this code on {subject}."
            if !reload_required {
                " This window closes automatically when pairing completes."
            }
        }
    };
    let actions = rsx! {
        if reload_required {
            Button {
                "data-testid": "pair-verification-code-reload",
                onclick: move |_| on_reload.call(()),
                "Reload page"
            }
        }
        Button {
            variant: ButtonVariant::Outline,
            "data-testid": "pair-verification-code-cancel",
            disabled: cancelling,
            "aria-busy": cancelling.to_string(),
            onclick: move |_| on_cancel.call(()),
            if cancelling { "Cancelling…" } else { "Cancel request" }
        }
    };
    rsx! {
        Dialog {
            open: true,
            on_close: move |()| on_cancel.call(()),
            headline: Some("Finish pairing".to_string()),
            description: Some(description),
            actions: Some(actions),
            test_id: Some("pair-verification-code-dialog".to_string()),
            show_close_button: Some(true),
            Surface {
                level: 2,
                radius: SurfaceRadius::Sm,
                pad: 4,
                border: true,
                test_id: Some("pair-verification-code".to_string()),
                role: "status",
                aria_live: "polite",
                aria_atomic: "true",
                code { class: "md-display-s pair-code-dialog__code",
                    {group_verification_code(&verification_code)}
                }
            }
            if reload_required {
                div { class: "pair-code-dialog__notice",
                    PairingStatusNotice {
                        tone: NoticeTone::Error,
                        message: "This page can no longer follow the pairing. Reload it to continue."
                            .to_string(),
                        test_id: None,
                    }
                }
            }
        }
    }
}

/// The dialog's host, mounted for the life of the document wherever the reader
/// walks.
///
/// v2 keys this on the approval record and renders it from the provider, beside
/// its children rather than inside them (`PairApprovalProvider.tsx:348-367`),
/// for one reason: an approver who reads a code aloud and then opens Settings
/// must not have the code disappear because a route changed. The reload the
/// `reload_required` state offers is the same code
/// `PairApprovalProvider.tsx:360` offers, and it is issued through
/// `platform::location::reload_document`, the crate's one document reload.
#[component]
pub fn ApprovalCodeDialog(approver: PairApprover) -> Element {
    let state = approver.state().read().clone();
    let Some(approval) = state.approval.clone() else {
        return rsx! {};
    };
    rsx! {
        PairVerificationCodeDialog {
            verification_code: approval.verification_code.clone(),
            requester_label: approval.requester_label.clone(),
            state: state.dialog,
            on_cancel: move |()| approver.cancel(),
            on_reload: move |_| crate::platform::location::reload_document(),
        }
    }
}
