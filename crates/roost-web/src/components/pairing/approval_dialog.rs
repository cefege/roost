//! The one place a generated verification code is shown. It is presentational:
//! the code, the lifecycle state, and every dismissal route into one cancel
//! callback that the approver turns into a `PairDeny`. There is no other close
//! affordance: a close that did not withdraw the approval would leave a live
//! six-digit code the coordinator still honours.

use dioxus::prelude::*;

use super::approver::{CodeDialogState, PairApprover};
use super::notices::{NoticeTone, PairingStatusNotice};
use super::status::group_verification_code;
use crate::components::ui::{
    Button, ButtonVariant, Dialog, DialogBody, DialogContent, DialogDescription, DialogFooter,
    DialogHeader, DialogTitle, Surface, SurfaceRadius,
};

/// The code, the state, and the two ways out of it.
#[component]
pub fn PairVerificationCodeDialog(
    verification_code: String,
    requester_label: String,
    state: CodeDialogState,
    on_cancel: EventHandler<()>,
    on_reload: EventHandler<()>,
) -> Element {
    let reload_required = state == CodeDialogState::ReloadRequired;
    let cancelling = state == CodeDialogState::Cancelling;
    let subject = if requester_label.trim().is_empty() {
        "the requesting browser".to_string()
    } else {
        requester_label.clone()
    };
    rsx! {
        Dialog {
            open: true,
            on_open_change: move |open: bool| {
                if !open {
                    on_cancel.call(());
                }
            },
            DialogContent { test_id: "pair-verification-code-dialog", show_close_button: true,
                DialogHeader {
                    DialogTitle { "Finish pairing" }
                    DialogDescription {
                        span {
                            "Enter this code on {subject}."
                            if !reload_required {
                                " This window closes automatically when pairing completes."
                            }
                        }
                    }
                }
                DialogBody {
                    Surface {
                        level: 2,
                        radius: SurfaceRadius::Sm,
                        pad: 4,
                        border: true,
                        test_id: Some("pair-verification-code".to_owned()),
                        role: Some("status".to_owned()),
                        aria_live: Some("polite".to_owned()),
                        aria_atomic: Some("true".to_owned()),
                        code { class: "block m-0 text-center text-3xl font-semibold text-foreground select-text",
                            {group_verification_code(&verification_code)}
                        }
                    }
                    if reload_required {
                        div { class: "mt-3",
                            PairingStatusNotice {
                                tone: NoticeTone::Error,
                                message: "This page can no longer follow the pairing. Reload it to continue."
                                    .to_string(),
                                test_id: None,
                            }
                        }
                    }
                }
                DialogFooter {
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
