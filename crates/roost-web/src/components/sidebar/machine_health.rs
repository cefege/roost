//! The folder row's machine-health readout: one compact line of the worker's
//! live CPU / memory / disk / network measurements, disclosed under the row.
//! The same heartbeat store the Settings → Machines cards read
//! (`store.workers`' `host_metrics`); a tiny per-machine projection, so the
//! sidebar answers "is this machine healthy" without a trip to Settings.
//!
//! The row renders one summary line while collapsed and the four-metric
//! compact grid while open; the disclosure is this row's own component state,
//! like its context menu. The metrics are `MetricTile` / `StatusDot`
//! primitives and the summary text is plain tokens, not hand-rolled chrome.

use dioxus::prelude::*;
use roost_client_core::store::navigation::worker_online;
use roost_protocol::wire::{HostMetrics, Worker};

use crate::components::md::metric_tile::MetricTile;
use crate::components::md::{Icon, StatusDot};
use crate::display_format::{format_bytes, format_speed};
use crate::pump::use_store;

/// The readout for one machine, under its folder row. The dot is the machine's
/// liveness — the same `worker_online` rule the Settings card and the row's
/// ServerChip use — not a claim about the metrics themselves.
#[component]
pub fn MachineHealth(worker: Option<Worker>) -> Element {
    let Some(worker) = worker else {
        return rsx! {};
    };
    let online = {
        let pump = use_store();
        let core = pump.core();
        let core = core.borrow();
        let now_ms = i64::try_from(core.clock().now_ms()).unwrap_or(i64::MAX);
        worker_online(&worker, core.store().routable_worker_fps.as_ref(), now_ms)
    };
    let mut open = use_signal(|| false);
    let metrics = worker.host_metrics.clone();
    let fingerprint = worker.fp.to_string();
    let summary = metrics.as_ref().map(machine_health_summary);
    rsx! {
        div { class: "df-machine-health", "data-testid": "machine-health-{fingerprint}",
            if let Some(summary) = summary {
                button {
                    class: "df-machine-health__toggle",
                    "aria-expanded": if open() { "true" } else { "false" },
                    title: "Machine health",
                    onclick: move |event: MouseEvent| {
                        event.stop_propagation();
                        event.prevent_default();
                        open.toggle();
                    },
                    span { class: "df-machine-health__pulse",
                        StatusDot {
                            status: if online { "ok" } else { "offline" },
                        }
                    }
                    span { class: "df-machine-health__summary", title: "CPU · memory · disk · network", {summary} }
                    Icon {
                        name: if open() { "expand_less" } else { "expand_more" },
                        size: crate::components::md::IconSize::Sm,
                        class: "df-machine-health__chevron".to_owned(),
                    }
                }
                if open() {
                    if let Some(live) = metrics {
                        div { class: "df-machine-health__tiles",
                            MachineHealthTiles { metrics: live }
                        }
                    }
                }
            }
        }
    }
}

/// The four compact tiles, the Machines card's grid at sidebar density.
#[component]
fn MachineHealthTiles(metrics: HostMetrics) -> Element {
    let memory_ratio = ratio(metrics.mem_used_bytes, metrics.mem_total_bytes);
    let disk_ratio = ratio(metrics.disk_used_bytes, metrics.disk_total_bytes);
    rsx! {
        MetricTile {
            icon: "memory".to_owned(),
            label: "CPU".to_owned(),
            value: format!("{:.0}%", metrics.cpu_pct),
            support: None,
            ratio: Some(metrics.cpu_pct / 100.0),
        }
        MetricTile {
            icon: "memory_alt".to_owned(),
            label: "Memory".to_owned(),
            value: percent(memory_ratio),
            support: Some(format!(
                "{} of {}",
                format_bytes(metrics.mem_used_bytes as f64),
                format_bytes(metrics.mem_total_bytes as f64)
            )),
            ratio: memory_ratio,
        }
        MetricTile {
            icon: "hard_drive".to_owned(),
            label: "Disk".to_owned(),
            value: percent(disk_ratio),
            support: Some(format!(
                "{} of {}",
                format_bytes(metrics.disk_used_bytes as f64),
                format_bytes(metrics.disk_total_bytes as f64)
            )),
            ratio: disk_ratio,
        }
        MetricTile {
            icon: "network_check".to_owned(),
            label: "Network".to_owned(),
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

/// A used/total pair as a fraction, or `None` when the machine reported no total.
fn ratio(used: i64, total: i64) -> Option<f64> {
    (total > 0).then(|| (used as f64 / total as f64).clamp(0.0, 1.0))
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |ratio| format!("{:.0}%", ratio * 100.0))
}

/// The collapsed summary: CPU, then the tightest of memory and disk.
fn machine_health_summary(metrics: &HostMetrics) -> String {
    let memory_ratio = ratio(metrics.mem_used_bytes, metrics.mem_total_bytes);
    let disk_ratio = ratio(metrics.disk_used_bytes, metrics.disk_total_bytes);
    let worst = memory_ratio
        .into_iter()
        .chain(disk_ratio)
        .fold(0.0_f64, f64::max);
    format!("{:.0}% · {:.0}%", metrics.cpu_pct, worst * 100.0)
}

#[cfg(test)]
mod tests {
    use super::{machine_health_summary, percent, ratio};
    use roost_protocol::wire::HostMetrics;

    fn metrics(
        cpu_pct: f64,
        mem_used: i64,
        mem_total: i64,
        disk_used: i64,
        disk_total: i64,
    ) -> HostMetrics {
        HostMetrics {
            cpu_pct,
            mem_used_bytes: mem_used,
            mem_total_bytes: mem_total,
            disk_used_bytes: disk_used,
            disk_total_bytes: disk_total,
            net_rx_bps: 0,
            net_tx_bps: 0,
            sampled_at_ms: 1,
        }
    }

    #[test]
    fn the_summary_reads_cpu_then_the_tightest_memory_or_disk_share() {
        let live = metrics(42.0, 8_000, 16_000, 2_000, 10_000);
        assert_eq!(machine_health_summary(&live), "42% · 50%");
    }

    #[test]
    fn disk_can_be_the_tighter_share() {
        let live = metrics(10.0, 1_000, 10_000, 9_000, 10_000);
        assert_eq!(machine_health_summary(&live), "10% · 90%");
    }

    #[test]
    fn a_machine_that_reported_no_totals_shows_only_cpu() {
        let live = metrics(7.5, 0, 0, 0, 0);
        assert_eq!(machine_health_summary(&live), "8% · 0%");
    }

    #[test]
    fn a_zero_total_is_no_ratio_not_a_divide() {
        assert_eq!(ratio(100, 0), None);
        assert_eq!(percent(None), "—");
        assert_eq!(percent(Some(0.5)), "50%");
    }
}
