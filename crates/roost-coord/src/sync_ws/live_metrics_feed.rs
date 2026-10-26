//! The live worker-metrics leg of one socket's feed: subscribes the live bus
//! and delivers each snapshot whose worker this socket has subscribed to.
//! Installed by `sync_ws::live_feed` beside the other eager subscriptions and
//! dropped with them, which is the unsubscribe. The membership it filters on is
//! `SyncResourceIndex::live_metric_fps`, set by `sync_ws::socket` from the
//! client's `worker_live_subscription`.
//! Frames are built by `sync_ws::feed::worker_frames`; they are unsequenced
//! controls, so nothing here stamps or retains them.

use std::sync::Arc;

use roost_protocol::wire::live_metrics::WorkerLiveMetricsFrame;

use crate::events::bus::Subscription;
use crate::events::bus_domains::Buses;
use crate::sync_ws::driver::{LinkState, SyncLink};
use crate::sync_ws::feed::FeedFrame;
use crate::sync_ws::feed::worker_frames::worker_live_metrics_frame;
use crate::sync_ws::live_feed::refused_as_none;

/// Subscribe the live metrics bus for `link`. The returned guard is held by
/// the socket's `LiveFeed` and unsubscribes when dropped.
pub(super) fn subscribe(
    link: &Arc<SyncLink>,
    buses: &Buses,
) -> Subscription<WorkerLiveMetricsFrame> {
    let sink = Arc::clone(link);
    buses.live_metrics_bus.subscribe(move |snapshot| {
        sink.deliver_with(|state| route_live_metrics(state, snapshot));
    })
}

/// One host snapshot, admitted only for a worker this socket subscribed to.
/// A snapshot for any other worker is dropped here, not in the bus, because
/// the subscription set is per socket.
fn route_live_metrics(
    state: &mut LinkState,
    snapshot: &WorkerLiveMetricsFrame,
) -> Option<FeedFrame> {
    if !state.index.live_metric_fps.contains(&snapshot.worker_fp) {
        return None;
    }
    refused_as_none("live_metrics_bus", worker_live_metrics_frame(snapshot))
}
