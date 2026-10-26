//! The Fleet page: one card per registered machine, with live readings while the
//! page is open. It subscribes to the online machines' metrics with replace
//! semantics, sends only when that set changes, and cancels on unmount.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::Store;
use roost_client_core::store::navigation::worker_online;
use roost_protocol::wire::live_metrics::LiveHostSnapshot;
use roost_protocol::wire::{Worker, WorkerFp};

use super::machine_card::MachineCard;
use super::readings::{ReadingState, reading_state};
use super::subscription::plan_subscription;
use crate::components::terminal::dom::sleep_ms;
use crate::components::ui::{
    Card, CardContent, CardDescription, CardHeader, CardTitle, EmptyState, IconType,
};
use crate::pump::{Pump, use_store};

/// How often the page re-checks staleness and reachability, so a machine whose
/// samples stop turns Stale without any new store traffic.
const RECHECK_MS: u64 = 1_000;

#[component]
pub fn FleetPage() -> Element {
    let pump = use_store();
    let mut last_sent = use_signal(Vec::<WorkerFp>::new);
    let mut recheck_tick = use_signal(|| 0_u64);

    use_effect({
        let pump = pump.clone();
        move || {
            let _revision = pump.revision()();
            let _recheck = recheck_tick();
            let now_ms = clock_now_ms(&pump);
            let online = online_fingerprints(&pump, now_ms);
            let planned = plan_subscription(&last_sent.peek(), online);
            if let Some(desired) = planned {
                tracing::info!(
                    target: "fleet",
                    machines = desired.len(),
                    "fleet live subscription changed"
                );
                last_sent.set(desired.clone());
                pump.dispatch(ClientEvent::WorkerLiveSubscribe {
                    worker_fps: desired,
                });
            }
        }
    });

    use_future(move || async move {
        loop {
            sleep_ms(RECHECK_MS).await;
            let next = recheck_tick.peek().wrapping_add(1);
            recheck_tick.set(next);
        }
    });

    use_drop({
        let pump = pump.clone();
        move || {
            pump.dispatch(ClientEvent::WorkerLiveSubscribe {
                worker_fps: Vec::new(),
            });
        }
    });

    let now_ms = clock_now_ms(&pump);
    let rows = fleet_rows(&pump, now_ms);
    let count = rows.len();
    rsx! {
        div {
            class: "flex flex-col gap-5",
            "data-testid": "fleet-page",
            Card {
                CardHeader {
                    CardTitle { "Fleet" }
                    CardDescription {
                        {if count == 1 { "1 machine".to_owned() } else { format!("{count} machines") }}
                    }
                }
            }
            if rows.is_empty() {
                Card {
                    CardContent {
                        EmptyState {
                            icon: IconType::Monitor,
                            title: "No machines registered yet",
                            supporting: "Register a machine to see its live readings here.",
                        }
                    }
                }
            } else {
                div { class: "grid grid-cols-[repeat(auto-fill,minmax(15rem,1fr))] gap-4",
                    for row in rows {
                        MachineCard {
                            key: "{row.fingerprint}",
                            label: row.label,
                            fingerprint: row.fingerprint,
                            state: row.state,
                            snapshot: row.snapshot,
                        }
                    }
                }
            }
        }
    }
}

/// One registered machine as the page renders it.
struct FleetRow {
    label: String,
    fingerprint: WorkerFp,
    state: ReadingState,
    snapshot: Option<LiveHostSnapshot>,
}

/// The clock's time in the store's signed millisecond domain.
fn clock_now_ms(pump: &Pump) -> i64 {
    let core = pump.core();
    let now = core.borrow().clock().now_ms();
    i64::try_from(now).unwrap_or(i64::MAX)
}

fn online_fingerprints(pump: &Pump, now_ms: i64) -> Vec<WorkerFp> {
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    store
        .workers
        .values()
        .filter(|worker| worker_online(worker, store.routable_worker_fps.as_ref(), now_ms))
        .map(|worker| worker.fp.clone())
        .collect()
}

/// Every registered machine, ordered by label then fingerprint. A live sample is
/// cloned only when it is Live, the one state that shows its readings.
fn fleet_rows(pump: &Pump, now_ms: i64) -> Vec<FleetRow> {
    let core = pump.core();
    let core = core.borrow();
    let store: &Store = core.store();
    let mut rows: Vec<FleetRow> = store
        .workers
        .values()
        .map(|worker| fleet_row(store, worker, now_ms))
        .collect();
    rows.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.fingerprint.cmp(&right.fingerprint))
    });
    rows
}

fn fleet_row(store: &Store, worker: &Worker, now_ms: i64) -> FleetRow {
    let online = worker_online(worker, store.routable_worker_fps.as_ref(), now_ms);
    let sample = store.worker_live.snapshots().get(&worker.fp);
    let state = reading_state(online, sample, now_ms);
    let snapshot = if state == ReadingState::Live {
        sample.cloned()
    } else {
        None
    };
    FleetRow {
        label: display_label(worker),
        fingerprint: worker.fp.clone(),
        state,
        snapshot,
    }
}

/// The worker's label, or the start of its fingerprint when it has none.
fn display_label(worker: &Worker) -> String {
    if worker.label.is_empty() {
        worker.fp.as_str().chars().take(12).collect()
    } else {
        worker.label.clone()
    }
}
