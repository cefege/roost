//! Rename and removal mutations for one machine settings row.
//! MachineRowView supplies its owned state and pump; canonical RPC results
//! determine busy, confirmation and error presentation.

use super::MachineRow;
use crate::pump::Pump;
use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::machines::{DeleteMachine, RenameMachine};
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;

pub(super) fn rename(pump: Pump, fingerprint: String, mut row: Signal<MachineRow>) {
    let label = row().label.trim().to_owned();
    if label.is_empty() {
        return;
    }
    row.write().busy = true;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let request = RenameMachine {
            fp: fingerprint,
            label,
        };
        match pump.rpc().call(&request).await {
            Ok(_) => {
                let mut row = row.write();
                row.busy = false;
                row.renaming = false;
                row.label.clear();
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "machine rename refused");
                let mut row = row.write();
                row.busy = false;
                row.error = Some(format!("Rename failed: {error}"));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, fingerprint, label, row);
}

pub(super) fn remove(pump: Pump, fingerprint: String, mut row: Signal<MachineRow>) {
    row.write().busy = true;
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let request = DeleteMachine { fp: fingerprint };
        let outcome = pump.rpc().call(&request).await;
        match outcome {
            Ok(true) => {
                let mut row = row.write();
                row.busy = false;
                row.confirming = false;
            }
            Ok(false) => {
                let mut row = row.write();
                row.busy = false;
                row.confirming = false;
                row.error = Some("Remove failed: the coordinator has no such machine".to_owned());
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "machine removal refused");
                let mut row = row.write();
                row.busy = false;
                row.confirming = false;
                row.error = Some(format!("Remove failed: {error}"));
                drop(row);
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed {
                    message: format!("Remove failed: {error}"),
                }));
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, fingerprint, row);
}
