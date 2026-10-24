//! One machine's row: its mark, its liveness, its update state, and the
//! controls that rename or remove it. Ports
//! `apps/web/src/components/Settings/MachineCard.tsx`; `machines::MachinesPane`
//! mounts one per registered worker.
//!
//! Row state is component state on purpose: it is one reader's open disclosure
//! and one in-flight write, and it has no meaning to any other row.

use dioxus::prelude::*;
use roost_client_core::store::navigation::worker_online;
use roost_protocol::fleet_update::{
    WorkerUpdateInputs, WorkerUpdateState, worker_update_label, worker_update_state,
};
use roost_protocol::wire::{HostMetrics, Worker};

use crate::components::machines::machine_identity_mark::MachineIdentityMark;
use crate::display_format::{format_bytes, format_speed};
use crate::pump::Pump;
use libero::components::{Button, Flex, Paper, Pictogram, ProgressBar, Text, TextField};

use super::super::format::{relative_time, short_identifier};
#[path = "machine_row_actions.rs"]
mod actions;
use actions::{remove, rename};

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
    let status_title = if online { "Online" } else { "Offline" }.to_owned();
    let address = worker
        .reachable_addr
        .clone()
        .unwrap_or_else(|| "Address unknown".to_owned());
    rsx! {
        Flex { direction: "column", gap: "sm", "data-testid": format!("machines-worker-row-{fingerprint}"),
            Flex { direction: "row", align: "center", justify: "space-between", gap: "md",
                Flex { direction: "row", align: "center", gap: "sm",
                    MachineIdentityMark { worker: Some(identity), context_title: None }
                    Flex { direction: "column", gap: "xs",
                        Text { size: "sm", "{worker.label}" }
                        Text { size: "xs", color: "muted", "{support_line(&worker, online, now_ms)}" }
                    }
                }
                Flex { direction: "row", align: "center", gap: "sm",
                    Text {
                        size: "xs",
                        color: if online { "primary" } else { "error" },
                        "aria-label": status_title.clone(),
                        title: status_title,
                        if online { "Online" } else { "Offline" }
                    }
                    Text {
                        size: "xs",
                        color: if update_state == WorkerUpdateState::UpdateAvailable { "primary" } else { "muted" },
                        "data-testid": format!("machines-update-state-{fingerprint}"),
                        title: format!("Coordinator release {}", short_identifier(coord_sha.as_deref().unwrap_or("unknown"), 8)),
                        "{worker_update_label(update_state)}"
                    }
                    Button {
                        variant: "outlined",
                        color: "error",
                        icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::trash_2::outlined } }),
                        "aria-label": format!("Remove {}", worker.label),
                        "data-testid": format!("machines-delete-quick-btn-{fingerprint}"),
                        disabled: state.busy,
                        onclick: move |_| row.write().confirming = true,
                    }
                    Button {
                        variant: "standard",
                        icon: Some(rsx! { Pictogram { icon: if state.details { pictogram_icons_lucide::chevron_up::outlined } else { pictogram_icons_lucide::chevron_down::outlined } } }),
                        "aria-expanded": state.details.to_string(),
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
                Flex { direction: "column", gap: "sm", id: format!("machines-worker-details-{fingerprint}"),
                    Flex { direction: "row", wrap: "wrap", gap: "sm",
                        Text { size: "xs", color: "muted", "{liveness_line(&worker, online, now_ms)}" }
                        Text { size: "xs", color: "muted", "{address}" }
                        Text { size: "xs", color: "muted", "Fingerprint {short_identifier(&fingerprint, 12)}" }
                        if let Some(sha) = worker.git_sha.clone() {
                            Text { size: "xs", color: "muted", "Worker version {short_identifier(&sha, 8)}" }
                        }
                    }
                    if state.renaming {
                        RenameForm { fingerprint: fingerprint.clone(), row, pump: pump.clone() }
                    }
                    if let Some(message) = state.error.clone() {
                        Text { color: "error", role: "alert", "{message}" }
                    }
                    if online {
                        if let Some(metrics) = worker.host_metrics.clone() {
                            MetricTiles { metrics }
                        }
                    }
                    Flex { direction: "row", align: "center", wrap: "wrap", gap: "sm",
                        Button {
                            variant: "standard",
                            icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::pencil::outlined } }),
                            "data-testid": format!("machines-rename-btn-{fingerprint}"),
                            onclick: move |_| {
                                let mut row = row.write();
                                row.renaming = !row.renaming;
                                row.error = None;
                            },
                            "Rename"
                        }
                        if state.confirming {
                            Text { size: "sm", color: "muted", "data-testid": format!("machines-delete-explanation-{fingerprint}"),
                                "Removes this credential; saved terminals and workspaces stay offline."
                            }
                            Button {
                                variant: "outlined",
                                color: "error",
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
                                variant: "standard",
                                "data-testid": format!("machines-cancel-delete-btn-{fingerprint}"),
                                onclick: move |_| row.write().confirming = false,
                                "Cancel"
                            }
                        } else {
                            Button {
                                variant: "outlined",
                                color: "error",
                                icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::trash_2::outlined } }),
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
    let rename_field_sx = use_hook(|| libero::sx::sx().flex("1"));
    rsx! {
        form {
            "data-testid": form_test_id,
            onsubmit: move |event| event.prevent_default(),
            Flex { direction: "row", align: "center", gap: "sm",
                TextField {
                    label: "Label",
                    sx: rename_field_sx,
                    value: state.label.clone(),
                    oninput: on_input,
                    "data-testid": "machines-rename-input",
                }
                Button {
                    variant: "standard",
                    "data-testid": "machines-rename-save",
                    disabled: state.busy || state.label.trim().is_empty(),
                    onclick: on_save,
                    "Save"
                }
                Button {
                    variant: "standard",
                    "data-testid": "machines-rename-cancel",
                    onclick: on_cancel,
                    "Cancel"
                }
            }
        }
    }
}

/// The live CPU, memory, disk and network measurements.
#[component]
fn MetricTiles(metrics: HostMetrics) -> Element {
    let memory_ratio = ratio(metrics.mem_used_bytes, metrics.mem_total_bytes);
    let disk_ratio = ratio(metrics.disk_used_bytes, metrics.disk_total_bytes);
    rsx! {
        Flex { direction: "row", wrap: "wrap", gap: "sm",
            MachineMetricTile {
                label: "CPU",
                value: format!("{:.0}%", metrics.cpu_pct),
                support: None,
                ratio: Some(metrics.cpu_pct / 100.0),
            }
            MachineMetricTile {
                label: "Memory",
                value: percent(memory_ratio),
                support: Some(format!(
                    "{} of {}",
                    format_bytes(metrics.mem_used_bytes as f64),
                    format_bytes(metrics.mem_total_bytes as f64)
                )),
                ratio: memory_ratio,
            }
            MachineMetricTile {
                label: "Disk",
                value: percent(disk_ratio),
                support: Some(format!(
                    "{} of {}",
                    format_bytes(metrics.disk_used_bytes as f64),
                    format_bytes(metrics.disk_total_bytes as f64)
                )),
                ratio: disk_ratio,
            }
            MachineMetricTile {
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

#[component]
fn MachineMetricTile(
    label: &'static str,
    value: String,
    support: Option<String>,
    ratio: Option<f64>,
) -> Element {
    rsx! {
        Paper {
            Flex { direction: "column", gap: "xs",
                Text { size: "xs", color: "muted", "{label}" }
                Text { size: "sm", "{value}" }
                if let Some(support) = support {
                    Text { size: "xs", color: "muted", "{support}" }
                }
                if let Some(ratio) = ratio {
                    ProgressBar { value: Some(ratio), max: 1.0, aria_label: format!("{label} usage") }
                }
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
