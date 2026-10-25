//! The Add Machine dialog: find the coordinator's enrollment door, and mint one
//! one-use join command once that door is diallable. Ports
//! `apps/web/src/components/machines/MachineDeployDialog.tsx`.
//!
//! THE GRANT IS MINTED LAST AND EXACTLY ONCE. Identity discovery runs on mount,
//! again on every "Check again", and once more inside Generate before any token
//! exists — because the declaration the operator is about to act on may have
//! changed since the dialog opened, and a grant minted against a door the
//! target cannot dial is a credential pasted into a shell that cannot work.
//! `deploy_state` owns those rules and `deploy_calls` owns the calls; this file
//! owns the wiring and the drawing.
//!
//! THERE IS NO PLATFORM CHOOSER. v2 offered one because its Windows branch ran
//! a different bootstrap; v3 ships Linux and macOS host installs only, and the
//! `install.sh` this command pipes detects the target's own system. A control that
//! changes nothing is a control nobody tested, so the dialog offers the label
//! the command actually carries and nothing else.

use dioxus::prelude::*;

use super::deploy_calls::{check_enrollment, mint_join_command};
use super::deploy_state::{DeployError, DeployModel, DeployPhase};
use super::local_access_guide::{MachineLocalAccessGuide, RECHECK_LABEL};
use crate::components::hooks::use_copy_clipboard::use_copy_clipboard;
use crate::components::ui::{
    Alert, AlertTitle, AlertVariant, Button, ButtonVariant, Card, CardContent, Dialog, DialogBody,
    DialogClose, DialogContent, DialogFooter, DialogHeader, DialogTitle, StatusDot, TextField,
};
use crate::pump::use_store;

/// The dialog. Mounted by the machines pane when the reader asks to add a
/// machine, and unmounted by that pane's dismissal.
#[component]
pub fn MachineDeployDialog(on_close: EventHandler<()>) -> Element {
    let pump = use_store();
    let model = use_signal(DeployModel::opened);
    let label = use_signal(String::new);
    let close = use_callback({
        let mut model = model;
        move |()| {
            model.write().close();
            on_close.call(());
        }
    });
    let recheck = use_callback({
        let pump = pump.clone();
        move |()| check_enrollment(pump.clone(), model)
    });
    let generate = use_callback({
        let pump = pump.clone();
        move |()| mint_join_command(pump.clone(), model, label().trim().to_owned())
    });
    let check_pump = pump.clone();
    use_effect(move || check_enrollment(check_pump.clone(), model));

    let state = model();
    let phase = state.phase();
    let (copy_command, copied) = use_copy_clipboard(None);
    // A re-check or a re-mint replaces the one-time command, so the
    // confirmation belongs to the command it copied.
    let mut copied_command = use_signal(|| None::<String>);
    let command_copied = copied() && copied_command.read().as_deref() == state.deploy_command();
    let guide = state.shows_local_access_guide();
    let minting = phase == DeployPhase::Minting;
    let generates = minting || phase == DeployPhase::Ready;
    let generated = phase == DeployPhase::Generated;
    let actions = rsx! {
        if generated {
            Button {
                variant: ButtonVariant::Secondary,
                "data-testid": "machine-deploy-copy",
                onclick: move |_| {
                    let command = model.peek().deploy_command().map(str::to_owned);
                    if let Some(command) = command {
                        copy_command(&command);
                        copied_command.set(Some(command));
                    }
                },
                if command_copied { "Copied" } else { "Copy command" }
            }
            DialogClose { "Done" }
        } else {
            DialogClose { "Cancel" }
            if generates {
                Button {
                    variant: ButtonVariant::Default,
                    "data-testid": "machine-deploy-generate",
                    disabled: minting,
                    onclick: move |_| generate.call(()),
                    if minting { "Generating…" } else { "Generate join command" }
                }
            }
            if phase == DeployPhase::Failed {
                Button {
                    variant: ButtonVariant::Secondary,
                    onclick: move |_| recheck.call(()),
                    {RECHECK_LABEL}
                }
            }
        }
    };
    let body = rsx! {
        if guide {
            MachineLocalAccessGuide {
                checking: phase == DeployPhase::Checking,
                on_recheck: move |()| recheck.call(()),
            }
        } else if generated {
            GeneratedCommand { model: state.clone() }
        } else {
            EnrollmentForm { model: state.clone(), label }
        }
    };
    rsx! {
        Dialog {
            open: true,
            on_open_change: move |open: bool| {
                if !open {
                    close.call(());
                }
            },
            DialogContent { test_id: "machine-deploy-dialog", show_close_button: true,
                DialogHeader {
                    DialogTitle { "Add Machine" }
                }
                DialogBody { {body} }
                DialogFooter { {actions} }
            }
        }
    }
}

/// The enrollment form: the address the worker will dial, the name it will
/// appear under, and any refusal.
#[component]
fn EnrollmentForm(model: DeployModel, mut label: Signal<String>) -> Element {
    let minting = model.phase() == DeployPhase::Minting;
    rsx! {
        div { class: "grid gap-4",
            if let Some(coordinator_url) = model.coordinator_url() {
                Card { size: crate::components::ui::CardSize::Sm,
                    CardContent {
                        div { class: "grid gap-4",
                            p { class: "m-0 font-medium text-muted-foreground", "Enrollment address" }
                            code { class: "break-all font-mono", {coordinator_url} }
                        }
                    }
                }
            }
            TextField {
                value: label(),
                on_input: move |value| label.set(value),
                label: Some("Machine label".to_owned()),
                placeholder: Some("optional — defaults to the machine's hostname".to_owned()),
                test_id: Some("machine-deploy-label".to_owned()),
                disabled: minting,
            }
            if let Some(error) = model.error() {
                DeployRefusal { error: error.clone() }
            }
        }
    }
}

/// The minted command and what to do with it.
#[component]
fn GeneratedCommand(model: DeployModel) -> Element {
    let Some(command) = model.deploy_command() else {
        return rsx! {};
    };
    let coordinator_url = model.coordinator_url().unwrap_or_default().to_owned();
    rsx! {
        div { class: "grid gap-4",
            p { class: "m-0 text-muted-foreground",
                {"Run this on the target machine, over its normal terminal, SSH session or cloud \
                  console. It must be able to reach "}
                code { class: "break-all font-mono", {coordinator_url} }
                {"."}
            }
            Card { size: crate::components::ui::CardSize::Sm,
                CardContent {
                    code { class: "break-all font-mono", {command} }
                }
            }
            p { class: "m-0 text-muted-foreground",
                "This one-use command expires after 24 hours. Treat it as a secret, and run it only \
                 on the machine you want to add."
            }
        }
    }
}

/// One refusal, named and actionable rather than a bare status code.
#[component]
fn DeployRefusal(error: DeployError) -> Element {
    let kind = error.kind;
    rsx! {
        Alert {
            variant: AlertVariant::Destructive,
            aria_live: "polite",
            aria_atomic: "true",
            div { class: "flex items-start gap-2",
                StatusDot { status: "error".to_owned(), title: Some("Error".to_owned()) }
                div { class: "grid gap-4", "data-machine-deploy-error-kind": kind.attribute(),
                    AlertTitle { {error.title} }
                    p { class: "m-0 text-muted-foreground", {error.detail} }
                }
            }
        }
    }
}
