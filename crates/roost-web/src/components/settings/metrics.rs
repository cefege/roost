//! Settings → System → Metrics: the coordinator's live telemetry counters.
//!
//! Ports `apps/web/src/components/Settings/MetricsPane.tsx`. Depends on
//! `roost-client-core`'s `MiscMetrics` call; the five-second poll is a timer
//! that skips a hidden tab, because a background settings page has no reader
//! and a poll nobody sees is a coordinator answering for nobody.

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::settings::metrics::GetMetrics;
use roost_client_core::client::rpc::calls::settings::metrics::{MetricsSnapshot, RouteCounters};

use crate::components::terminal::dom::{page_visible, sleep_ms};
use crate::pump::{Pump, use_store};
use libero::components::{Flex, Paper, Pictogram, SvgData, Table, Text, column};

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

    let semibold_text_sx = use_hook(|| libero::sx::sx().font_weight("600"));
    let medium_text_sx = use_hook(|| libero::sx::sx().font_weight("500"));
    let state = view();
    let paper_sx = use_hook(|| libero::sx::sx().padding("md"));
    rsx! {
        Flex { direction: "column", gap: "md", "data-testid": "metrics-pane",
            Paper { sx: paper_sx.clone(),
                Flex { direction: "column", gap: "sm",
                    Text { size: "lg", sx: semibold_text_sx.clone(), "Coordinator metrics" }
                    Text { size: "sm", color: "muted", "Live telemetry counters. Refreshes every 5s. Counts reset on coord restart." }
                    if let Some(message) = state.error.clone() {
                        Flex { direction: "row", align: "center", gap: "xs", "data-testid": "metrics-error",
                            Pictogram { icon: pictogram_icons_lucide::circle_alert::outlined }
                            Text { size: "sm", color: "error", role: "alert", "{message}" }
                        }
                    }
                    match state.snapshot.clone() {
                        Some(snapshot) => rsx! {
                            Flex { direction: "row", wrap: "wrap", gap: "sm", "data-testid": "metrics-summary",
                                MetricTile { icon: pictogram_icons_lucide::clock::outlined, label: "Uptime", value: format_uptime(snapshot.uptime_ms), support: None }
                                MetricTile { icon: pictogram_icons_lucide::refresh_cw::outlined, label: "Requests", value: group(snapshot.total_requests), support: None }
                                MetricTile {
                                    icon: pictogram_icons_lucide::triangle_alert::outlined,
                                    label: "Errors",
                                    value: group(snapshot.total_errors),
                                    support: Some(if snapshot.total_errors > 0 { "4xx + 5xx responses" } else { "All clear" }.to_owned()),
                                }
                                MetricTile { icon: pictogram_icons_lucide::trending_down::outlined, label: "Error rate", value: error_rate(&snapshot), support: None }
                            }
                        },
                        None if state.loading => rsx! { Text { size: "sm", color: "muted", "Loading…" } },
                        None => rsx! {},
                    }
                }
            }
            Paper { sx: paper_sx,
                Flex { direction: "column", gap: "sm",
                    Text { size: "lg", sx: semibold_text_sx.clone(), "Per-route activity" }
                    if state.snapshot.as_ref().is_some_and(|snapshot| snapshot.routes.is_empty()) {
                        Flex { direction: "column", align: "center", gap: "xs",
                            Pictogram { icon: pictogram_icons_lucide::activity::outlined }
                            Text { size: "md", sx: semibold_text_sx.clone(), "No requests recorded yet" }
                            Text { size: "sm", color: "muted", "Counters reset on coord restart. Drive some traffic and metrics will appear here within 5 s." }
                        }
                    } else {
                        Table {
                            data: state.snapshot.clone().unwrap_or_default().routes,
                            row_attrs: |_: &_| vec![Attribute::new("data-testid", "metrics-row", None, false)],
                            columns: vec![
                                column("Route")
                                    .value(|route: &RouteCounters| route.path.clone())
                                    .render({
                                        let medium_text_sx = medium_text_sx.clone();
                                        move |route| {
                                            rsx! { Text { size: "sm", sx: medium_text_sx.clone(), "{route.path}" } }
                                        }
                                    }),
                                column("Requests").value(|route: &RouteCounters| group(route.requests)).render(|route| rsx! { Text { size: "sm", color: "muted", "{group(route.requests)}" } }),
                                column("Errors").value(|route: &RouteCounters| group(route.errors)).render(|route| rsx! { Text { size: "sm", color: "muted", "{group(route.errors)}" } }),
                                column("Error rate").value(|route: &RouteCounters| route_error_percent(route.requests, route.errors)).render(|route| rsx! { Text { size: "sm", color: "muted", "{route_error_percent(route.requests, route.errors)}" } }),
                            ],
                        }
                    }
                }
            }
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

#[component]
fn MetricTile(
    icon: SvgData,
    label: &'static str,
    value: String,
    support: Option<String>,
) -> Element {
    let semibold_text_sx = use_hook(|| libero::sx::sx().font_weight("600"));
    let paper_sx = use_hook(|| libero::sx::sx().padding("sm"));
    rsx! {
        Paper { sx: paper_sx,
            Flex { direction: "column", gap: "xs",
                Flex { direction: "row", align: "center", gap: "xs",
                    Pictogram { icon }
                    Text { size: "xs", color: "muted", "{label}" }
                }
                Text { size: "lg", sx: semibold_text_sx.clone(), "{value}" }
                if let Some(support) = support {
                    Text { size: "xs", color: "muted", "{support}" }
                }
            }
        }
    }
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
