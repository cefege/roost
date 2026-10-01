//! One machine's row: its mark, its liveness, its update state, and the
//! controls that rename or remove it. Ports
//! `apps/web/src/components/Settings/MachineCard.tsx`; `machines::MachinesPane`
//! mounts one per registered worker.
//!
//! Row state is component state on purpose: it is one reader's open disclosure
//! and one in-flight write, and it has no meaning to any other row.

use dioxus::prelude::*;
use roost_client_core::store::navigation::worker_online;
// The round trips are browser-only: a native build has no coordinator to ask,
// so the call types and the toast their refusals raise are gated with them.
#[cfg(target_arch = "wasm32")]
use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::machines::{DeleteMachine, RenameMachine};
#[cfg(target_arch = "wasm32")]
use roost_client_core::store::shell_intent::ShellIntent;
use roost_protocol::fleet_update::{
    WorkerUpdateInputs, WorkerUpdateState, worker_update_label, worker_update_state,
};
use roost_protocol::wire::{HostMetrics, Worker};

use crate::components::machines::machine_identity_mark::MachineIdentityMark;
use crate::components::md::{
    Button, ButtonVariant, Chip, ListRow, MetricTile, StatusDot, TextField,
};
use crate::display_format::{format_bytes, format_speed};
use crate::pump::Pump;

use super::super::format::{relative_time, short_identifier};

/// What this row's own controls are doing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MachineRow {
    /// Whether the details disclosure is open.
    pub details: bool,
    /// Whether the rename form is showing.
    pub renaming: bool,
    /// The label being typed.
    pub label: String,
    /// Whether the removal is waiting for its confirm.
    pub confirming: bool,
    /// Whether a write is in flight.
    pub busy: bool,
    /// The last refusal, shown beside the controls that caused it.
    pub error: Option<String>,
}

/// The row.
#[component]
pub fn MachineRowView(
    worker: Worker,
    coord_sha: Option<String>,
    now_ms: i64,
    pump: Pump,
) -> Element {
    let mut row = use_signal(MachineRow::default);
    let core = pump.core();
    let online = {
        let core = core.borrow();
        worker_online(&worker, core.store().routable_worker_fps.as_ref(), now_ms)
    };
    let update_state = worker_update_state(WorkerUpdateInputs {
        worker_git_sha: worker.git_sha.as_deref(),
        coord_git_sha: coord_sha.as_deref(),
        online,
        deploy_in_flight: false,
    });
    let fingerprint = worker.fp.to_string();
    let identity = worker.clone();
    let state = row();
    let status = if online { "ok" } else { "offline" }.to_owned();
    let status_title = if online { "Online" } else { "Offline" }.to_owned();
    rsx! {
        div { class: "machines-worker", "data-testid": format!("machines-worker-row-{fingerprint}"),
            ListRow {
                leading: rsx! { MachineIdentityMark { worker: Some(identity), context_title: None } },
                headline: rsx! { {worker.label.clone()} },
                support: rsx! { {support_line(&worker, online, now_ms)} },
                trailing: rsx! {
                    StatusDot {
                        status: status,
                        title: Some(status_title),
                    }
                    Chip {
                        label: worker_update_label(update_state).to_owned(),
                        selected: Some(update_state == WorkerUpdateState::UpdateAvailable),
                        test_id: Some(format!("machines-update-state-{fingerprint}")),
                        title: Some(format!("Coordinator release {}", short_identifier(
                            coord_sha.as_deref().unwrap_or("unknown"), 8
                        ))),
                    }
                    Button {
                        variant: ButtonVariant::Destructive,
                        icon: "delete_outline",
                        "aria-label": format!("Remove {}", worker.label),
                        "data-testid": format!("machines-delete-quick-btn-{fingerprint}"),
                        disabled: state.busy,
                        onclick: move |_| row.write().confirming = true,
                    }
                    Button {
                        variant: ButtonVariant::Ghost,
                        icon: if state.details { "expand_less" } else { "expand_more" },
                        "aria-expanded": state.details,
                        onclick: move |_| {
                            let mut row = row.write();
                            row.details = !row.details;
                            row.error = None;
                        },
                        "Details"
                    }
                }
            }
            if state.details {
                div { class: "machines-worker-details", id: format!("machines-worker-details-{fingerprint}"),
                    div { class: "machines-worker-details__identity",
                        span { {liveness_line(&worker, online, now_ms)} }
                        span { {worker.reachable_addr.clone().unwrap_or_else(|| "Address unknown".to_owned())} }
                        span { {format!("Fingerprint {}", short_identifier(&fingerprint, 12))} }
                        if let Some(sha) = worker.git_sha.clone() {
                            span { {format!("Worker version {}", short_identifier(&sha, 8))} }
                        }
                    }
                    if state.renaming {
                        RenameForm { fingerprint: fingerprint.clone(), row, pump: pump.clone() }
                    }
                    if let Some(message) = state.error.clone() {
                        p { class: "md-body-s", style: "color: var(--md-sys-color-error);", {message} }
                    }
                    if online {
                        if let Some(metrics) = worker.host_metrics.clone() {
                            MetricTiles { metrics }
                        }
                    }
                    div { class: "machines-worker-details__actions",
                        Button {
                            variant: ButtonVariant::Ghost,
                            icon: "edit",
                            "data-testid": format!("machines-rename-btn-{fingerprint}"),
                            onclick: move |_| {
                                let mut row = row.write();
                                row.renaming = !row.renaming;
                                row.error = None;
                            },
                            "Rename"
                        }
                        if state.confirming {
                            span { class: "md-body-s", "data-testid": format!("machines-delete-explanation-{fingerprint}"),
                                "Removes this credential; saved terminals and workspaces stay offline."
                            }
                            Button {
                                variant: ButtonVariant::Destructive,
                                "data-testid": format!("machines-confirm-delete-btn-{fingerprint}"),
                                disabled: state.busy,
                                onclick: {
                                    let fingerprint = fingerprint.clone();
                                    let pump = pump.clone();
                                    move |_| remove(pump.clone(), fingerprint.clone(), row)
                                },
                                "Confirm remove"
                            }
                            Button {
                                variant: ButtonVariant::Ghost,
                                "data-testid": format!("machines-cancel-delete-btn-{fingerprint}"),
                                onclick: move |_| row.write().confirming = false,
                                "Cancel"
                            }
                        } else {
                            Button {
                                variant: ButtonVariant::Destructive,
                                icon: "delete_outline",
                                "data-testid": format!("machines-delete-btn-{fingerprint}"),
                                disabled: state.busy,
                                onclick: move |_| row.write().confirming = true,
                                "Remove"
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The inline rename form, which only a reader who asked to rename sees.
#[component]
fn RenameForm(fingerprint: String, mut row: Signal<MachineRow>, pump: Pump) -> Element {
    let state = row();
    let form_test_id = format!("machines-rename-form-{fingerprint}");
    let on_input = move |value: String| row.write().label = value;
    let on_save = move |_event: MouseEvent| rename(pump.clone(), fingerprint.clone(), row);
    let on_cancel = move |_event: MouseEvent| {
        let mut row = row.write();
        row.renaming = false;
        row.error = None;
    };
    rsx! {
        form {
            class: "settings-inline-form",
            style: "display: flex; gap: var(--md-space-2); align-items: center;",
            "data-testid": form_test_id,
            onsubmit: move |event| event.prevent_default(),
            TextField {
                test_id: "machines-rename-input",
                label: "Label",
                style: "flex: 1; min-width: 0;",
                value: state.label.clone(),
                on_input: on_input,
            }
            Button {
                variant: ButtonVariant::Default,
                "data-testid": "machines-rename-save",
                disabled: state.busy || state.label.trim().is_empty(),
                onclick: on_save,
                "Save"
            }
            Button {
                variant: ButtonVariant::Ghost,
                "data-testid": "machines-rename-cancel",
                onclick: on_cancel,
                "Cancel"
            }
        }
    }
}

/// The live CPU, memory, disk and network tiles.
#[component]
fn MetricTiles(metrics: HostMetrics) -> Element {
    let memory_ratio = ratio(metrics.mem_used_bytes, metrics.mem_total_bytes);
    let disk_ratio = ratio(metrics.disk_used_bytes, metrics.disk_total_bytes);
    rsx! {
        div { class: "md-metric-grid",
            MetricTile {
                icon: "memory",
                label: "CPU",
                value: format!("{:.0}%", metrics.cpu_pct),
                support: None,
                ratio: Some(metrics.cpu_pct / 100.0),
            }
            MetricTile {
                icon: "memory_alt",
                label: "Memory",
                value: percent(memory_ratio),
                support: Some(format!(
                    "{} of {}",
                    format_bytes(metrics.mem_used_bytes as f64),
                    format_bytes(metrics.mem_total_bytes as f64)
                )),
                ratio: memory_ratio,
            }
            MetricTile {
                icon: "hard_drive",
                label: "Disk",
                value: percent(disk_ratio),
                support: Some(format!(
                    "{} of {}",
                    format_bytes(metrics.disk_used_bytes as f64),
                    format_bytes(metrics.disk_total_bytes as f64)
                )),
                ratio: disk_ratio,
            }
            MetricTile {
                icon: "network_check",
                label: "Network",
                value: format_speed((metrics.net_rx_bps + metrics.net_tx_bps) as f64),
                support: Some(format!(
                    "↓ {} · ↑ {}",
                    format_speed(metrics.net_rx_bps as f64),
                    format_speed(metrics.net_tx_bps as f64)
                )),
                ratio: None,
            }
        }
    }
}

/// A used/total pair as a fraction, or `None` when the machine reported no total.
fn ratio(used: i64, total: i64) -> Option<f64> {
    (total > 0).then(|| (used as f64 / total as f64).clamp(0.0, 1.0))
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |ratio| format!("{:.0}%", ratio * 100.0))
}

/// The support line: how quiet the machine is, and where it can be reached.
fn support_line(worker: &Worker, online: bool, now_ms: i64) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !online {
        parts.push(liveness_line(worker, online, now_ms));
    }
    if let Some(address) = &worker.reachable_addr {
        parts.push(address.clone());
    }
    parts.join(" · ")
}

fn liveness_line(worker: &Worker, online: bool, now_ms: i64) -> String {
    if online {
        return "Live connection".to_owned();
    }
    let last_seen = u64::try_from(worker.last_seen_ms).unwrap_or_default();
    let now = u64::try_from(now_ms).unwrap_or_default();
    format!("Last seen {}", relative_time(now, last_seen))
}

/// Rename one machine.
fn rename(pump: Pump, fingerprint: String, mut row: Signal<MachineRow>) {
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

/// Remove one machine from the fleet.
fn remove(pump: Pump, fingerprint: String, mut row: Signal<MachineRow>) {
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
