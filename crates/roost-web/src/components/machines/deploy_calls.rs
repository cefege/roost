//! The coordinator calls the deploy dialog makes, and the fences around them.
//!
//! Split from `deploy_dialog` so every entry point sits beside the work it
//! owns. Each one starts its operation on the model — which mints the
//! generation — and hands the answer back to `DeployModel`, which is what
//! decides whether that answer still has a dialog to land on. Nothing here
//! decides policy; `deploy_state` does.
//!
//! The round trips are browser-only. A native build has no coordinator to ask,
//! so it keeps the one state that is true without one: the dialog is checking,
//! and nothing has been decided.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::machines::{
    GetCoordinatorIdentity, MintWorkerBootstrap,
};
#[cfg(target_arch = "wasm32")]
use roost_platform::machine_join_command;

#[cfg(target_arch = "wasm32")]
use super::deploy_state::DeployError;
use super::deploy_state::DeployModel;
#[cfg(target_arch = "wasm32")]
use super::enrollment_origin::enrollment_decision;
use crate::components::notifications::clipboard;
use crate::pump::Pump;

/// Ask the coordinator which address it enrolls new machines against.
///
/// Nothing caches a discovery read, so "Check again" really is a new question:
/// the operator may have redeployed their front door since the last one, and a
/// dialog that answered from its own memory would offer a door that is gone.
pub fn check_enrollment(pump: Pump, mut model: Signal<DeployModel>) {
    let generation = model.write().begin_check();
    #[cfg(target_arch = "wasm32")]
    run_check(pump, model, generation);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, generation, model);
}

#[cfg(target_arch = "wasm32")]
fn run_check(pump: Pump, mut model: Signal<DeployModel>, generation: u64) {
    wasm_bindgen_futures::spawn_local(async move {
        let rpc = pump.rpc();
        match rpc.call(&GetCoordinatorIdentity).await {
            Ok(declared) => {
                let decision = enrollment_decision(&declared, rpc.base_url());
                model.write().accept_identity(generation, decision);
            }
            Err(error) => {
                tracing::warn!(target: "machines", %error, "enrollment address check refused");
                refuse(&mut model, generation, &error.to_string());
            }
        }
    });
}

/// Mint ONE grant, after re-checking the door it would be spent against.
///
/// The re-check is the point: the address this dialog opened with may be minutes
/// old, and a grant minted against a door the target cannot dial is a credential
/// pasted into a shell that cannot work. `begin_mint` is what makes a second
/// press a no-op rather than a second grant.
pub fn mint_join_command(pump: Pump, mut model: Signal<DeployModel>, label: String) {
    let Some(generation) = model.write().begin_mint() else {
        return;
    };
    #[cfg(target_arch = "wasm32")]
    run_mint(pump, model, generation, label);
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, generation, model, label);
}

#[cfg(target_arch = "wasm32")]
fn run_mint(pump: Pump, mut model: Signal<DeployModel>, generation: u64, label: String) {
    wasm_bindgen_futures::spawn_local(async move {
        let rpc = pump.rpc();
        let declared = match rpc.call(&GetCoordinatorIdentity).await {
            Ok(declared) => declared,
            Err(error) => {
                tracing::warn!(target: "machines", %error, "enrollment address check refused");
                refuse(&mut model, generation, &error.to_string());
                return;
            }
        };
        let decision = enrollment_decision(&declared, rpc.base_url());
        if !model.write().accept_identity(generation, decision) {
            return;
        }
        let Some(coordinator_url) = model.write().coordinator_url().map(str::to_owned) else {
            return;
        };
        let request = MintWorkerBootstrap {
            label: label.clone(),
        };
        match rpc.call(&request).await {
            Ok(grant) => {
                let command = machine_join_command(&coordinator_url, &grant.token, &label);
                model.write().accept_mint(generation, command);
            }
            Err(error) => {
                tracing::warn!(target: "machines", %error, "worker grant mint refused");
                refuse(&mut model, generation, &error.to_string());
            }
        }
    });
}

/// Put the minted command on the clipboard.
///
/// The answer is fenced by the dialog's generation like every other one, so a
/// write that resolves after the reader walked away cannot relabel a dialog that
/// is no longer on screen.
pub fn copy_command(mut model: Signal<DeployModel>) {
    let (generation, command) = {
        let state = model.read();
        (
            state.generation(),
            state.deploy_command().map(str::to_owned),
        )
    };
    let Some(command) = command else {
        return;
    };
    let copied = clipboard::copy_text(&command);
    model.write().accept_copy(generation, copied);
}

#[cfg(target_arch = "wasm32")]
fn refuse(model: &mut Signal<DeployModel>, generation: u64, detail: &str) {
    model
        .write()
        .accept_failure(generation, DeployError::coordinator(detail));
}
