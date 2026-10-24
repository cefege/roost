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
use crate::components::overlay_generation::OverlayGeneration;
use crate::pump::use_store;
use libero::components::{Alert, Button, Code, Dialog, Flex, Paper, Text, TextField};
use libero::hooks::{ModalScope, use_modal};
use libero::sx::sx;
use std::cell::RefCell;
use std::rc::Rc;

/// The dialog. Mounted by the machines pane when the reader asks to add a
/// machine, and unmounted by that pane's dismissal.
#[component]
pub fn MachineDeployDialog(on_close: EventHandler<()>) -> Element {
    let pump = use_store();
    let mut model = use_signal(DeployModel::opened);
    let label = use_signal(String::new);
    let generation = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
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
    let (copy_command, copied) = use_copy_clipboard(None);
    let mut copied_command = use_signal(|| None::<String>);
    let modal = use_modal(move |scope: ModalScope<u64>| {
        if scope.try_args().is_none() {
            return rsx! {};
        }
        let state = model();
        let phase = state.phase();
        let guide = state.shows_local_access_guide();
        let minting = phase == DeployPhase::Minting;
        let generates = minting || phase == DeployPhase::Ready;
        let generated = phase == DeployPhase::Generated;
        let command_copied = copied() && copied_command.read().as_deref() == state.deploy_command();
        let local_recheck = recheck;
        let local_generate = generate;
        let copy_command = copy_command.clone();

        rsx! {
            Dialog { title: "Add Machine", size: "md",
                "data-testid": "machine-deploy-dialog",
                ondismiss: move |_| true,
                Flex { direction: "column", gap: "md",
                    if guide {
                        MachineLocalAccessGuide {
                            checking: phase == DeployPhase::Checking,
                            on_recheck: move |()| local_recheck.call(()),
                        }
                    } else if generated {
                        GeneratedCommand { model: state.clone() }
                    } else {
                        EnrollmentForm { model: state.clone(), label }
                    }
                    if generated {
                        Button {
                            variant: "outlined",
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
                        Button {
                            variant: "filled",
                            onclick: move |_| scope.close(),
                            "Done"
                        }
                    } else {
                        Button { variant: "standard", color: "muted", onclick: move |_| scope.close(), "Cancel" }
                        if generates {
                            Button {
                                variant: "filled",
                                "data-testid": "machine-deploy-generate",
                                disabled: minting,
                                loading: minting,
                                onclick: move |_| local_generate.call(()),
                                if minting { "Generating…" } else { "Generate join command" }
                            }
                        }
                        if phase == DeployPhase::Failed {
                            Button {
                                variant: "outlined",
                                onclick: move |_| local_recheck.call(()),
                                {RECHECK_LABEL}
                            }
                        }
                    }
                }
            }
        }
    });
    let open_modal = modal;
    let modal_generation = Rc::clone(&generation);
    use_effect(move || {
        modal_generation.borrow_mut().invalidate();
        let token = modal_generation.borrow_mut().begin();
        let completion_generation = Rc::clone(&modal_generation);
        open_modal.open_with(token).onresult(move |_| {
            if completion_generation.borrow_mut().finish(token) {
                model.write().close();
                on_close.call(());
            }
        });
    });
    let teardown_generation = Rc::clone(&generation);
    use_drop(move || teardown_generation.borrow_mut().invalidate());
    rsx! {}
}

/// The enrollment form: the address the worker will dial, the name it will
/// appear under, and any refusal.
#[component]
fn EnrollmentForm(model: DeployModel, mut label: Signal<String>) -> Element {
    let address_code_sx = use_hook(|| {
        sx().with("overflow-wrap", "anywhere")
            .font_family("monospace")
    });
    let minting = model.phase() == DeployPhase::Minting;
    rsx! {
        Flex { direction: "column", gap: "md",
            if let Some(coordinator_url) = model.coordinator_url() {
                Paper {
                    Text { size: "sm", color: "muted", "Enrollment address" }
                    Code { source: coordinator_url, sx: address_code_sx.clone() }
                }
            }
            TextField {
                value: Some(label()),
                oninput: move |value| label.set(value),
                label: "Machine label",
                placeholder: "optional — defaults to the machine's hostname",
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
    let inline_code_sx = use_hook(|| {
        sx().with("overflow-wrap", "anywhere")
            .font_family("monospace")
    });
    let command_sx = use_hook(|| {
        sx().with("overflow-wrap", "anywhere")
            .font_family("monospace")
    });
    rsx! {
        Flex { direction: "column", gap: "md",
            Text { size: "sm", color: "muted",
                {"Run this on the target machine, over its normal terminal, SSH session or cloud \
                  console. It must be able to reach "}
                Code { source: coordinator_url, sx: inline_code_sx }
                {"."}
            }
            Paper { Code { source: command, sx: command_sx } }
            Text { size: "sm", color: "muted",
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
            title: error.title,
            color: "error",
            "data-machine-deploy-error-kind": kind.attribute(),
            {error.detail}
        }
    }
}
