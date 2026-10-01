//! Settings → System → Metrics: the coordinator's live telemetry counters.
//!
//! Ports `apps/web/src/components/Settings/MetricsPane.tsx`. Depends on
//! `roost-client-core`'s `MiscMetrics` call; the five-second poll is a timer
//! that skips a hidden tab, because a background settings page has no reader
//! and a poll nobody sees is a coordinator answering for nobody.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::metrics::GetMetrics;
use roost_client_core::client::rpc::calls::settings::metrics::MetricsSnapshot;

use crate::components::md::{Card, EmptyState, Icon, MetricTile};
use crate::components::terminal::dom::{page_visible, sleep_ms};
use crate::pump::{Pump, use_store};

/// How often the counters are re-read, in milliseconds.
const POLL_INTERVAL_MS: u64 = 5_000;
/// The snapshot, and whatever the last read was told.
#[derive(Debug, Clone, PartialEq, Default)]
struct MetricsView {
    snapshot: Option<MetricsSnapshot>,
    error: Option<String>,
    loading: bool,
}

/// The pane.
#[component]
pub fn MetricsPane() -> Element {
    let pump = use_store();
    let view = use_signal(|| MetricsView {
        loading: true,
        ..MetricsView::default()
    });
    // One loop over one clock: the first read and every tick after it are the
    // same call, so a pane cannot paint a snapshot from a poll nobody made.
    use_future(move || {
        let pump = pump.clone();
        async move {
            poll(pump.clone(), view);
            loop {
                sleep_ms(POLL_INTERVAL_MS).await;
                if page_visible() {
                    poll(pump.clone(), view);
                }
            }
        }
    });

    let state = view();
    rsx! {
        div {
            class: "settings-pane",
            style: "display: flex; flex-direction: column; gap: var(--md-space-5);",
            "data-testid": "metrics-pane",
            Card {
                title: "Coordinator metrics",
                supporting: "Live telemetry counters. Refreshes every 5s. Counts reset on coord restart.",
                if let Some(message) = state.error.clone() {
                    div { "data-testid": "metrics-error",
                        style: "display: flex; align-items: center; gap: var(--md-space-2);",
                        Icon { name: "error", style: "color: var(--md-sys-color-error);" }
                        span { class: "md-body-m", style: "color: var(--md-sys-color-error);", {message} }
                    }
                }
                match state.snapshot.clone() {
                    Some(snapshot) => rsx! {
                        div { class: "md-metric-grid", "data-testid": "metrics-summary",
                            MetricTile { icon: "schedule", label: "Uptime", value: format_uptime(snapshot.uptime_ms), support: None, ratio: None }
                            MetricTile { icon: "sync_alt", label: "Requests", value: group(snapshot.total_requests), support: None, ratio: None }
                            MetricTile {
                                icon: "report",
                                label: "Errors",
                                value: group(snapshot.total_errors),
                                support: Some(if snapshot.total_errors > 0 { "4xx + 5xx responses" } else { "All clear" }.to_owned()),
                                ratio: None,
                            }
                            MetricTile { icon: "trending_down", label: "Error rate", value: error_rate(&snapshot), support: None, ratio: None }
                        }
                    },
                    None if state.loading => rsx! {
                        span { class: "md-body-m", style: "color: var(--md-sys-color-on-surface-variant);", "Loading…" }
                    },
                    None => rsx! {},
                }
            }
            Card { title: "Per-route activity",
                if state.snapshot.as_ref().is_some_and(|snapshot| snapshot.routes.is_empty()) {
                    EmptyState {
                        icon: "monitoring",
                        title: "No requests recorded yet",
                        supporting: "Counters reset on coord restart. Drive some traffic and metrics will appear here within 5 s.",
                    }
                } else {
                    for route in state
                        .snapshot
                        .clone()
                        .unwrap_or_default()
                        .routes
                        .iter()
                        .map(|route| {
                            (
                                route.path.clone(),
                                group(route.requests),
                                group(route.errors),
                                route_error_percent(route.requests, route.errors),
                            )
                        })
                        .collect::<Vec<_>>()
                    {
                        RouteRow { path: route.0, requests: route.1, errors: route.2, error_percent: route.3 }
                    }
                }
            }
        }
    }
}

/// One route's counters. Positional rather than keyed so a five-second poll
/// updates the text it changed instead of rebuilding every row.
#[component]
fn RouteRow(path: String, requests: String, errors: String, error_percent: String) -> Element {
    rsx! {
        div { class: "md-list-row", "data-testid": "metrics-row", "data-route": path.clone(),
            span { style: "font-family: var(--font-mono); flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis;",
                title: path.clone(),
                {path.clone()}
            }
            span { style: "font-family: var(--font-mono); color: var(--md-sys-color-on-surface-variant);", {requests.clone()} }
            span { style: "font-family: var(--font-mono); color: var(--md-sys-color-on-surface-variant);", {errors.clone()} }
            span { style: "font-family: var(--font-mono); color: var(--md-sys-color-on-surface-variant);", {error_percent.clone()} }
        }
    }
}

/// Re-read the counters.
fn poll(pump: Pump, view: Signal<MetricsView>) {
    #[cfg(target_arch = "wasm32")]
    wasm_bindgen_futures::spawn_local(async move {
        let mut view = view;
        match pump.rpc().call(&GetMetrics).await {
            Ok(snapshot) => {
                let mut view = view.write();
                view.snapshot = Some(snapshot);
                view.error = None;
                view.loading = false;
            }
            Err(error) => {
                tracing::warn!(target: "settings", %error, "metrics read refused");
                let mut view = view.write();
                view.error = Some(error.to_string());
                view.loading = false;
            }
        }
    });
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, view);
}

/// v2's `fmtUptime`.
fn format_uptime(ms: u64) -> String {
    let seconds = ms / 1_000;
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let remainder = seconds % 60;
    if hours > 0 {
        return format!("{hours}h {minutes}m {remainder}s");
    }
    if minutes > 0 {
        return format!("{minutes}m {remainder}s");
    }
    format!("{remainder}s")
}

/// A counter with thousands separators, without a locale dependency.
fn group(value: u64) -> String {
    let digits = value.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(character);
    }
    grouped
}

fn error_rate(snapshot: &MetricsSnapshot) -> String {
    if snapshot.total_requests == 0 {
        return "—".to_owned();
    }
    format!(
        "{:.2}%",
        f64::from(u32::try_from(snapshot.total_errors).unwrap_or(u32::MAX))
            / f64::from(u32::try_from(snapshot.total_requests).unwrap_or(u32::MAX))
            * 100.0
    )
}

fn route_error_percent(requests: u64, errors: u64) -> String {
    if requests == 0 {
        return "—".to_owned();
    }
    format!("{:.1}%", errors as f64 / requests as f64 * 100.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_as_v2_did() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(61_000), "1m 1s");
        assert_eq!(format_uptime(3_661_000), "1h 1m 1s");
    }

    #[test]
    fn counters_group_by_thousands() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1_000), "1,000");
        assert_eq!(group(1_234_567), "1,234,567");
    }

    #[test]
    fn a_route_with_no_requests_has_no_rate() {
        assert_eq!(route_error_percent(0, 0), "—");
        assert_eq!(route_error_percent(200, 50), "25.0%");
    }
}
