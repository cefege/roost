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

use super::deploy_calls::{check_enrollment, copy_command, mint_join_command};
use super::deploy_state::{DeployError, DeployModel, DeployPhase};
use super::local_access_guide::{MachineLocalAccessGuide, RECHECK_LABEL};
use crate::components::md::{
    Button, ButtonVariant, Dialog, StatusDot, Surface, SurfaceRadius, TextField,
};
use crate::pump::use_store;

const STACK_STYLE: &str = "display: grid; gap: var(--md-space-4);";
const SUPPORT_STYLE: &str = "margin: 0; color: var(--md-sys-color-on-surface-variant);";
const COMMAND_STYLE: &str = "overflow-wrap: anywhere;";
const ERROR_ROW_STYLE: &str = "display: flex; align-items: flex-start; gap: var(--md-space-2);";
const ERROR_TITLE_STYLE: &str = "margin: 0; color: var(--md-sys-color-error);";

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
    let copied = state.copied();
    let guide = state.shows_local_access_guide();
    let minting = phase == DeployPhase::Minting;
    let generates = minting || phase == DeployPhase::Ready;
    let generated = phase == DeployPhase::Generated;
    let actions = rsx! {
        if generated {
            Button {
                variant: ButtonVariant::Secondary,
                "data-testid": "machine-deploy-copy",
                onclick: move |_| copy_command(model),
                if copied { "Copied" } else { "Copy command" }
            }
            Button {
                variant: ButtonVariant::Outline,
                onclick: move |_| close.call(()),
                "Done"
            }
        } else {
            Button {
                variant: ButtonVariant::Outline,
                onclick: move |_| close.call(()),
                "Cancel"
            }
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
            on_close: move |()| close.call(()),
            headline: Some("Add Machine".to_owned()),
            test_id: Some("machine-deploy-dialog".to_owned()),
            show_close_button: Some(true),
            actions: Some(actions),
            {body}
        }
    }
}

/// The enrollment form: the address the worker will dial, the name it will
/// appear under, and any refusal.
#[component]
fn EnrollmentForm(model: DeployModel, mut label: Signal<String>) -> Element {
    let minting = model.phase() == DeployPhase::Minting;
    rsx! {
        div { style: STACK_STYLE,
            if let Some(coordinator_url) = model.coordinator_url() {
                Surface { level: 1, radius: SurfaceRadius::Sm, pad: 3, border: true,
                    div { style: STACK_STYLE,
                        p { class: "md-label-m", style: SUPPORT_STYLE, "Enrollment address" }
                        code { class: "md-body-m", style: COMMAND_STYLE, {coordinator_url} }
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
        div { style: STACK_STYLE,
            p { class: "md-body-m", style: SUPPORT_STYLE,
                {"Run this on the target machine, over its normal terminal, SSH session or cloud \
                  console. It must be able to reach "}
                code { style: COMMAND_STYLE, {coordinator_url} }
                {"."}
            }
            Surface { level: 1, radius: SurfaceRadius::Sm, pad: 3, border: true,
                code { class: "md-body-s", style: COMMAND_STYLE, {command} }
            }
            p { class: "md-body-m", style: SUPPORT_STYLE,
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
        Surface { level: 1, radius: SurfaceRadius::Sm, pad: 3, border: true,
            role: Some("alert".to_owned()),
            aria_live: Some("polite".to_owned()),
            aria_atomic: Some("true".to_owned()),
            div { style: ERROR_ROW_STYLE,
                StatusDot { status: "error".to_owned(), title: Some("Error".to_owned()) }
                div { style: STACK_STYLE, "data-machine-deploy-error-kind": kind.attribute(),
                    p { class: "md-title-s", style: ERROR_TITLE_STYLE, {error.title} }
                    p { class: "md-body-s", style: SUPPORT_STYLE, {error.detail} }
                }
            }
        }
    }
}
