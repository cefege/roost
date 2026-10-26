//! Live worker metrics: the hub that tells each worker when a Sync socket wants
//! its host snapshots, and the path snapshots take back to those sockets.
//!
//! Called by `sync_ws::socket` (a socket's subscription set changed),
//! `sync_ws::socket_open` (a socket closed), and `worker_link::link_session`
//! (a worker reconnected). Nothing here is persisted: a live metric is latest
//! only and never written to the database.

mod hub;

use std::collections::BTreeSet;
use std::sync::MutexGuard;

use roost_proto::DLiveMetricsSubscription;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::services::CoordServices;

pub use hub::{LiveMetricsHub, SubscriptionChange};

/// Replace one socket's subscription set and tell each worker whose count
/// crossed zero. The hub lock is held across the sends so two sockets cannot
/// interleave their `active` frames out of order.
pub fn replace_socket_subscriptions(
    services: &CoordServices,
    socket_id: &str,
    wanted: BTreeSet<WorkerFp>,
) {
    let mut hub = lock_hub(services);
    let changes = hub.replace(socket_id, wanted);
    announce_changes(services, socket_id, &changes);
}

/// Release every subscription a closing socket held.
pub fn release_socket(services: &CoordServices, socket_id: &str) {
    let mut hub = lock_hub(services);
    let changes = hub.release_socket(socket_id);
    announce_changes(services, socket_id, &changes);
}

/// A worker that just (re)connected must sample again if any socket still
/// wants it: the subscription outlived the link that carried it.
pub fn rearm_worker(services: &CoordServices, handle: &WorkerHandle) {
    let hub = lock_hub(services);
    let subscribers = hub.subscriber_count(&handle.worker_fp);
    if subscribers == 0 {
        return;
    }
    let delivered = send_active(handle, true);
    tracing::info!(
        event = "live_metrics",
        action = "link_rearmed",
        worker_fp = %handle.worker_fp,
        subscribers,
        delivered,
        "live metrics re-armed on a reconnected worker"
    );
}

fn lock_hub(services: &CoordServices) -> MutexGuard<'_, LiveMetricsHub> {
    services
        .live_metrics
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn announce_changes(services: &CoordServices, socket_id: &str, changes: &[SubscriptionChange]) {
    for change in changes {
        let delivered = services
            .workers
            .current_routable(&change.worker_fp)
            .is_some_and(|handle| send_active(&handle, change.active));
        let action = if change.active {
            "subscribe_started"
        } else {
            "subscribe_stopped"
        };
        tracing::info!(
            event = "live_metrics",
            action,
            socket_id,
            worker_fp = %change.worker_fp,
            delivered,
            "live metrics subscription changed"
        );
    }
}

fn send_active(handle: &WorkerHandle, active: bool) -> bool {
    handle.send(CoordWorkerDownstream::LiveMetricsSubscription(
        DLiveMetricsSubscription {
            active,
            ..Default::default()
        },
    )) != 0
}
