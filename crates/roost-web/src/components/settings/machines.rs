//! Settings → Network → Machines: the registered fleet, one row each.
//!
//! Ports `apps/web/src/components/Settings/MachinesPane.tsx`; each row is
//! `machine_row::MachineRowView`, the port of `MachineCard.tsx`. The list comes
//! from the store's worker registry, which is the Sync feed's projection.
//!
//! "Add machine" mounts `machines::deploy_dialog::MachineDeployDialog` for as
//! long as the reader is enrolling one, so the dialog's identity read, its mint
//! and every late answer die with the pane rather than with the document.
//!
//! Update state is the ONE classifier, `roost_protocol::fleet_update`; neither
//! this file nor its row compares SHAs itself.

pub mod machine_row;

use dioxus::prelude::*;
use roost_protocol::wire::Worker;

use crate::components::machines::deploy_dialog::MachineDeployDialog;
use crate::components::md::{Button, ButtonVariant, Card, EmptyState, List};
use crate::pump::use_store;

/// The pane.
#[component]
pub fn MachinesPane() -> Element {
    let pump = use_store();
    let mut enrolling = use_signal(|| false);
    let core = pump.core();
    let (workers, coord_sha, now_ms) = {
        let core = core.borrow();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        let mut workers: Vec<Worker> = core.store().workers.values().cloned().collect();
        workers.sort_by_key(|worker| std::cmp::Reverse(worker.last_seen_ms));
        let sha = core
            .store()
            .coord_identity
            .as_ref()
            .map(|identity| identity.git_sha.clone());
        (workers, sha, now_ms)
    };
    let count = workers.len();
    rsx! {
        div {
            class: "settings-pane",
            "data-testid": "settings-machines-pane",
            Card {
                title: "Machines",
                supporting: if count == 1 { "1 machine".to_owned() } else { format!("{count} machines") },
                trailing: rsx! {
                    Button {
                        variant: ButtonVariant::Default,
                        icon: "add",
                        "data-testid": "machines-add-btn",
                        onclick: move |_| {
                            enrolling.set(true);
                            tracing::info!(target: "settings", "machine enrollment dialog opened");
                        },
                        "Add machine"
                    }
                },
                if workers.is_empty() {
                    EmptyState {
                        icon: "desktop_mac",
                        title: "No machines yet",
                        supporting: "Add a machine to start spawning sessions.",
                    }
                } else {
                    List { contained: true,
                        for worker in workers {
                            machine_row::MachineRowView {
                                worker,
                                coord_sha: coord_sha.clone(),
                                now_ms,
                                pump: pump.clone(),
                            }
                        }
                    }
                }
            }
            if enrolling() {
                MachineDeployDialog { on_close: move |_| enrolling.set(false) }
            }
        }
    }
}
