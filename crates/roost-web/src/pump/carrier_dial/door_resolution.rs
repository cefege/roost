//! Which worker door a grant dials: the page's one discovery, started at boot
//! and joined by every later dial.
//!
//! Called by `pump::carrier_dial::open` and by `pump::boot` before the first
//! dial. Decisions are `client::local::discovery::DoorDiscovery`'s; the probe
//! fetch is `platform::door_probe`.

use futures_util::FutureExt;
use roost_client_core::client::local::bootstrap::LocalBootstrap;
use roost_client_core::client::local::discovery::{
    BrowserEnvironment, DoorAdoption, DoorPlan, LocalWorkerDoor,
};

use crate::platform::door_probe;
use crate::pump::Pump;

/// The in-flight (or settled) door discovery every dial on this page joins.
pub(in crate::pump) type DoorProbe = futures_util::future::Shared<
    futures_util::future::LocalBoxFuture<'static, Option<LocalWorkerDoor>>,
>;

/// The door this page can dial, running discovery at most once.
///
/// Started by `pump::boot` before the first dial, so a grant that lands later
/// joins the probe already in flight (or its answer) instead of opening a
/// network wait of its own. The shared future is only the join point; whether
/// this page has asked is still `DoorDiscovery`'s answer.
pub(in crate::pump) async fn resolve_door(pump: &Pump) -> Option<LocalWorkerDoor> {
    if let Some(door) = pump.inner.carriers.borrow().door().cloned() {
        return Some(door);
    }
    let probe = pump
        .inner
        .door_probe
        .borrow_mut()
        .get_or_insert_with(|| {
            let pump = pump.clone();
            async move { discover_door(&pump).await }
                .boxed_local()
                .shared()
        })
        .clone();
    probe.await
}

/// Run discovery once. `DoorDiscovery` refuses a second `start` precisely so a
/// page that mints for two panes probes once.
async fn discover_door(pump: &Pump) -> Option<LocalWorkerDoor> {
    let page_origin = door_probe::page_origin();
    let served_by_worker = served_by_this_page();
    let operator_origin = door_probe::stored_operator_origin();
    let plan = pump
        .inner
        .carriers
        .borrow_mut()
        .doors()
        .start(&BrowserEnvironment {
            page_origin: page_origin.clone(),
            served_by_worker,
            operator_origin,
        });
    match plan {
        DoorPlan::Adopting(door) => Some(door),
        DoorPlan::Probe { origin, url } => {
            let answer = door_probe::fetch_bootstrap(&url).await;
            let adoption = pump.inner.carriers.borrow_mut().doors().complete_probe(
                &origin,
                answer.status,
                &answer.body,
            );
            match adoption {
                DoorAdoption::Adopted(door) => Some(door),
                DoorAdoption::Absent(absence) => {
                    tracing::info!(
                        target: "carriers",
                        origin = %origin,
                        reason = absence.reason(),
                        "no worker door on this machine; the session stays on Sync"
                    );
                    None
                }
            }
        }
        DoorPlan::NotProbed(absence) => {
            tracing::info!(
                target: "carriers",
                reason = absence.reason(),
                "no worker door probe on this page; the session stays on Sync"
            );
            None
        }
        // `start` refuses a second call, and this page has just made its first
        // with no door behind it, so the adoption arrived on a path that already
        // recorded it. The table is the answer.
        DoorPlan::AlreadyAttempted => pump.inner.carriers.borrow().door().cloned(),
    }
}

/// The bootstrap this page's OWN origin serves, if it serves one.
///
/// Read before discovery decides, because that is the fact `DoorPlan::Adopting`
/// turns on: a page a worker served never probes anything. The answer is the
/// one `door_probe::prime_serving_bootstrap` recorded before `dioxus::launch`,
/// so it is final before any grant can arrive and costs no second request.
fn served_by_this_page() -> Option<LocalBootstrap> {
    door_probe::primed_serving_bootstrap()
}
