//! Aligned CPU, RAM and disk meters for one machine, with optional network rates.
//! Fleet live samples and Settings heartbeat readings share these ratios and
//! labels; each surface converts its source through `HostUsage`.

use dioxus::prelude::*;
use roost_protocol::wire::HostMetrics;
use roost_protocol::wire::live_metrics::LiveHostSnapshot;

use crate::components::ui::Progress;
use crate::display_format::{format_bytes, format_speed};

/// Whole-host usage in the units the meters render.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HostUsage {
    pub cpu_pct: f64,
    pub mem_used_bytes: f64,
    pub mem_total_bytes: f64,
    pub disk_used_bytes: f64,
    pub disk_total_bytes: f64,
}

impl HostUsage {
    pub fn from_host_metrics(metrics: &HostMetrics) -> Self {
        Self {
            cpu_pct: metrics.cpu_pct,
            mem_used_bytes: metrics.mem_used_bytes as f64,
            mem_total_bytes: metrics.mem_total_bytes as f64,
            disk_used_bytes: metrics.disk_used_bytes as f64,
            disk_total_bytes: metrics.disk_total_bytes as f64,
        }
    }

    pub fn from_live(snapshot: &LiveHostSnapshot) -> Self {
        Self {
            cpu_pct: snapshot.cpu_pct,
            mem_used_bytes: snapshot.mem_used_bytes as f64,
            mem_total_bytes: snapshot.mem_total_bytes as f64,
            disk_used_bytes: snapshot.disk_used_bytes as f64,
            disk_total_bytes: snapshot.disk_total_bytes as f64,
        }
    }
}

/// Network throughput in bytes per second.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NetworkRates {
    pub rx_bps: f64,
    pub tx_bps: f64,
}

impl NetworkRates {
    pub fn from_host_metrics(metrics: &HostMetrics) -> Self {
        Self {
            rx_bps: metrics.net_rx_bps as f64,
            tx_bps: metrics.net_tx_bps as f64,
        }
    }
}

/// Host resource bars with aligned labels and values; narrow layouts stack the bars.
#[component]
pub fn HostUsageMeters(usage: HostUsage, network: Option<NetworkRates>) -> Element {
    let memory_ratio = ratio(usage.mem_used_bytes, usage.mem_total_bytes);
    let disk_ratio = ratio(usage.disk_used_bytes, usage.disk_total_bytes);
    rsx! {
        div {
            class: "flex min-w-0 flex-col gap-2",
            role: "group",
            "aria-label": "Machine host metrics",
            "data-testid": "machines-host-metrics",
            ResourceMeter {
                label: "CPU",
                value: Some(usage.cpu_pct / 100.0),
                reading: format!("{:.0}%", usage.cpu_pct),
            }
            ResourceMeter {
                label: "RAM",
                value: memory_ratio,
                reading: format!(
                    "{} · {} / {}",
                    percent(memory_ratio),
                    format_bytes(usage.mem_used_bytes),
                    format_bytes(usage.mem_total_bytes),
                ),
            }
            ResourceMeter {
                label: "Disk",
                value: disk_ratio,
                reading: format!(
                    "{} · {} / {}",
                    percent(disk_ratio),
                    format_bytes(usage.disk_used_bytes),
                    format_bytes(usage.disk_total_bytes),
                ),
            }
            if let Some(rates) = network {
                div { class: "flex items-center justify-between gap-3 text-xs text-muted-foreground",
                    span { class: "w-12 shrink-0", "Network" }
                    span { class: "text-right tabular-nums",
                        "RX {format_speed(rates.rx_bps)} · TX {format_speed(rates.tx_bps)}"
                    }
                }
            }
        }
    }
}

#[component]
fn ResourceMeter(label: &'static str, value: Option<f64>, reading: String) -> Element {
    rsx! {
        div { class: "flex min-w-0 flex-wrap items-center gap-x-3 gap-y-1 text-xs md:flex-nowrap",
            span { class: "w-12 shrink-0 text-muted-foreground", {label} }
            Progress {
                value,
                label: label.to_owned(),
                meter: true,
                class: Some("order-3 w-full md:order-none md:flex-1".to_owned()),
            }
            span { class: "order-2 min-w-0 flex-1 text-right tabular-nums text-muted-foreground md:order-none md:w-40 md:flex-none",
                {reading}
            }
        }
    }
}

/// A used/total pair as a fraction, or `None` when the machine reported no total.
fn ratio(used: f64, total: f64) -> Option<f64> {
    (total > 0.0).then(|| (used / total).clamp(0.0, 1.0))
}

fn percent(value: Option<f64>) -> String {
    value.map_or_else(|| "—".to_owned(), |ratio| format!("{:.0}%", ratio * 100.0))
}
