//! One registered machine's card on the Fleet page. It shows readings only in
//! the Live state; Stale shows the sample's age, Awaiting shows placeholders,
//! and Offline shows a plain notice.

use dioxus::prelude::*;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::live_metrics::LiveHostSnapshot;

use super::process_table::ProcessTable;
use super::readings::{ReadingState, format_uptime};
use crate::components::machines::host_usage::{HostUsage, HostUsageMeters};
use crate::components::ui::{
    Badge, BadgeVariant, Card, CardAction, CardContent, CardDescription, CardHeader, CardSize,
    CardTitle, Collapsible, CollapsibleContent, CollapsibleTrigger, Skeleton, StatusDot,
};

#[component]
pub fn MachineCard(
    label: String,
    fingerprint: WorkerFp,
    state: ReadingState,
    snapshot: Option<LiveHostSnapshot>,
) -> Element {
    let short_fingerprint: String = fingerprint.as_str().chars().take(12).collect();
    rsx! {
        Card {
            size: CardSize::Sm,
            "data-testid": format!("fleet-card-{fingerprint}"),
            CardHeader {
                CardTitle { "{label}" }
                CardDescription { "{short_fingerprint}" }
                CardAction { StateBadge { state } }
            }
            {match (state, snapshot) {
                (ReadingState::Live, Some(sample)) => rsx! { LiveReadings { sample } },
                (ReadingState::Stale { age_secs }, _) => rsx! {
                    CardContent {
                        p {
                            class: "text-sm text-muted-foreground",
                            "Last sample {age_secs}s ago. Readings stay hidden until a fresh sample arrives."
                        }
                    }
                },
                (ReadingState::Awaiting, _) => rsx! {
                    CardContent {
                        div { class: "flex flex-col gap-3",
                            Skeleton { class: "h-16 w-full" }
                            Skeleton { class: "h-4 w-1/2" }
                        }
                    }
                },
                _ => rsx! {
                    CardContent {
                        p {
                            class: "text-sm text-muted-foreground",
                            "Not reachable. Readings resume when it comes back online."
                        }
                    }
                },
            }}
        }
    }
}

#[component]
fn StateBadge(state: ReadingState) -> Element {
    match state {
        ReadingState::Live => rsx! {
            StatusDot { status: "ok".to_owned(), title: Some("Live".to_owned()) }
        },
        ReadingState::Stale { .. } => rsx! { Badge { variant: BadgeVariant::Warning, "Stale" } },
        ReadingState::Awaiting => {
            rsx! { Badge { variant: BadgeVariant::Muted, "Awaiting sample" } }
        }
        ReadingState::Offline => rsx! {
            StatusDot { status: "offline".to_owned(), title: Some("Offline".to_owned()) }
        },
    }
}

#[component]
fn LiveReadings(sample: LiveHostSnapshot) -> Element {
    let mut expanded = use_signal(|| false);
    let uptime = format_uptime(sample.uptime_secs);
    let usage = HostUsage::from_live(&sample);
    rsx! {
        CardContent {
            div { class: "flex flex-col gap-4",
                HostUsageMeters { usage, network: None }
                div { class: "flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground",
                    span { "Up {uptime}" }
                    span { "{sample.process_count} processes" }
                    span { "{sample.cpu_count} CPUs" }
                }
                Collapsible {
                    open: expanded(),
                    on_open_change: move |next: bool| expanded.set(next),
                    CollapsibleTrigger { "Processes" }
                    CollapsibleContent {
                        ProcessTable { processes: sample.processes }
                    }
                }
            }
        }
    }
}
