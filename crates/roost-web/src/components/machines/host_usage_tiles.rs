//! The CPU, memory, disk and network tiles for one machine's host readings.
//! Shared by the settings machine row and the Fleet cards so both read the same
//! ratios and labels. Each caller converts its own source into [`HostUsage`].

use dioxus::prelude::*;
use roost_protocol::wire::HostMetrics;
use roost_protocol::wire::live_metrics::LiveHostSnapshot;

use crate::components::ui::{IconType, MetricTile};
use crate::display_format::{format_bytes, format_speed};

/// Whole-host usage in the units the tiles render.
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

/// The CPU, memory, disk and, when the source reports it, network tiles.
#[component]
pub fn HostUsageTiles(usage: HostUsage, network: Option<NetworkRates>) -> Element {
    let memory_ratio = ratio(usage.mem_used_bytes, usage.mem_total_bytes);
    let disk_ratio = ratio(usage.disk_used_bytes, usage.disk_total_bytes);
    rsx! {
        div { class: "grid grid-cols-2 gap-3",
            MetricTile {
                icon: Some(IconType::Cpu),
                label: "CPU",
                value: format!("{:.0}%", usage.cpu_pct),
                support: None,
                ratio: Some(usage.cpu_pct / 100.0),
            }
            MetricTile {
                icon: Some(IconType::MemoryStick),
                label: "Memory",
                value: percent(memory_ratio),
                support: Some(format!(
                    "{} of {}",
                    format_bytes(usage.mem_used_bytes),
                    format_bytes(usage.mem_total_bytes)
                )),
                ratio: memory_ratio,
            }
            MetricTile {
                icon: Some(IconType::HardDrive),
                label: "Disk",
                value: percent(disk_ratio),
                support: Some(format!(
                    "{} of {}",
                    format_bytes(usage.disk_used_bytes),
                    format_bytes(usage.disk_total_bytes)
                )),
                ratio: disk_ratio,
            }
            if let Some(rates) = network {
                MetricTile {
                    icon: Some(IconType::Network),
                    label: "Network",
                    value: format_speed(rates.rx_bps + rates.tx_bps),
                    support: Some(format!(
                        "↓ {} · ↑ {}",
                        format_speed(rates.rx_bps),
                        format_speed(rates.tx_bps)
                    )),
                    ratio: None,
                }
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
