//! The one place a generated verification code is shown. The approver's state
//! owns the approval; this host mirrors each approval into one fenced Libero
//! modal and routes every dismissal through the server-side cancellation path.

use std::{cell::RefCell, rc::Rc};

use dioxus::prelude::*;
use libero::{
    components::{Button, Dialog, Flex, Paper, Text},
    hooks::{ModalScope, use_modal},
    sx::sx,
};

use roost_client_core::client::auth::PairApproval;

use super::approver::{CodeDialogState, PairApprover};
use super::notices::{NoticeTone, PairingStatusNotice};
use super::status::group_verification_code;
use crate::components::overlay_generation::OverlayGeneration;

/// The code dialog's host, mounted above route and access-gate changes.
#[component]
pub fn ApprovalCodeDialog(approver: PairApprover) -> Element {
    let state = approver.state().read().clone();
    let approval = state.approval.clone();
    let previous = use_hook(|| Rc::new(RefCell::new(None::<PairApproval>)));
    let body_sx = use_hook(|| sx().gap("md"));

    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let modal_approver = approver.clone();
    let modal_generation = Rc::clone(&generation);
    let modal_body_sx = body_sx.clone();
    let dialog = use_modal(move |scope: ModalScope<(PairApproval, u64)>| {
        let Some((approval, token)) = scope.try_args() else {
            return rsx! {};
        };
        let state = modal_approver.state().read().clone();
        let dismiss_approver = modal_approver.clone();
        let button_generation = Rc::clone(&modal_generation);
        let button_approver = modal_approver.clone();
        let dismiss_generation = Rc::clone(&modal_generation);
        rsx! {
            Dialog {
                title: "Finish pairing",
                close_button: Some(true),
                "data-testid": "pair-verification-code-dialog",
                ondismiss: move |_| {
                    if dismiss_generation.borrow().is_current(token)
                        && state.dialog != CodeDialogState::Cancelling
                    {
                        dismiss_approver.cancel();
                    }
                    false
                },
                Flex { direction: "column", sx: modal_body_sx.clone(),
                    Text { size: "sm", color: "muted",
                        "Enter this code on "
                        {if approval.requester_label.trim().is_empty() { "the requesting browser" } else { &approval.requester_label }}
                        ". This window closes automatically when pairing completes."
                    }
                    Paper { bordered: true, role: "status", aria_live: "polite", aria_atomic: "true",
                        Text { component: "code", size: "xl", "data-testid": "pair-verification-code",
                            {group_verification_code(&approval.verification_code)}
                        }
                    }
                    if state.dialog == CodeDialogState::ReloadRequired {
                        PairingStatusNotice {
                            tone: NoticeTone::Error,
                            message: "This page can no longer follow the pairing. Reload it to continue.".to_owned(),
                            test_id: None,
                        }
                    }
                    Flex { direction: "row", gap: "sm", justify: "end",
                        if state.dialog == CodeDialogState::ReloadRequired {
                            Button {
                                "data-testid": "pair-verification-code-reload",
                                onclick: move |_| crate::platform::location::reload_document(),
                                "Reload page"
                            }
                        }
                        Button {
                            variant: "outlined",
                            "data-testid": "pair-verification-code-cancel",
                            disabled: state.dialog == CodeDialogState::Cancelling,
                            "aria-busy": (state.dialog == CodeDialogState::Cancelling).to_string(),
                            onclick: move |_| {
                                if button_generation.borrow().is_current(token) {
                                    button_approver.cancel();
                                }
                            },
                            if state.dialog == CodeDialogState::Cancelling { "Cancelling…" } else { "Cancel request" }
                        }
                    }
                }
            }
        }
    });
    let sync_dialog = dialog;
    let sync_generation = Rc::clone(&generation);
    let sync_previous = Rc::clone(&previous);
    use_effect(use_reactive!(|(approval)| {
        {
            let mut previous = sync_previous.borrow_mut();
            if *previous == approval {
                return;
            }
            *previous = approval.clone();
        }
        sync_generation.borrow_mut().invalidate();
        sync_dialog.close();
        if let Some(approval) = approval {
            let token = sync_generation.borrow_mut().begin();
            let completion_generation = Rc::clone(&sync_generation);
            sync_dialog.open_with((approval, token)).onresult(move |_| {
                completion_generation.borrow_mut().finish(token);
            });
        }
    }));
    let teardown_generation = Rc::clone(&generation);
    use_drop(move || teardown_generation.borrow_mut().invalidate());
    rsx! {}
}
