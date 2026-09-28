//! What a ready worker link owes its worker between frames: the kills queued
//! for it while it was away, and the delayed respawn-if-missing pass.
//!
//! Called by `worker_link::link_session` on its timer and after each dispatch.
//! Ports the owed-reap delivery and `scheduleRespawn` of
//! `apps/coord/src/workers/worker-conn.ts:151-160`.

use std::sync::Arc;
use std::time::Duration;

use roost_protocol::wire::SessionId;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::services::CoordServices;
use crate::worker_link::reap_outbox::ReapOutbox;
use crate::workers::respawn::respawn_missing_for_worker;
use crate::workers::send::reap_orphan_pty;

/// How long after readiness the respawn-if-missing pass runs: long enough for
/// the worker's own post-snapshot replay to land first.
pub(super) const RESPAWN_DELAY: Duration = Duration::from_secs(3);

/// Carry the kills this worker is owed, once its generation is routable.
pub(super) fn deliver_owed_reaps(
    services: &CoordServices,
    handle: &WorkerHandle,
    reaps: &ReapOutbox,
) {
    if !handle.is_routable() {
        return;
    }
    for kill in reaps.take_pending() {
        match SessionId::try_from(kill.session_id.as_str()) {
            Ok(session_id) => {
                reap_orphan_pty(&services.workers, &kill.worker_fp, &session_id);
            }
            Err(error) => tracing::warn!(worker_fp = %kill.worker_fp, %error,
                "worker link: an owed reap names no addressable session"),
        }
    }
}

/// Offer the worker every open session it still owns, off the read loop.
pub(super) fn spawn_respawn(services: &Arc<CoordServices>, handle: &Arc<WorkerHandle>) {
    let services = Arc::clone(services);
    let handle = Arc::clone(handle);
    tokio::spawn(async move {
        if !handle.is_routable() {
            return;
        }
        let report = respawn_missing_for_worker(
            &services.db,
            &services.workers,
            &*services.views,
            &services.write_gate(),
            &handle,
        )
        .await;
        tracing::info!(worker_fp = %handle.worker_fp, dispatched = report.dispatched,
            skipped = report.skipped, deferred = report.deferred,
            "worker link: respawn-if-missing pass finished");
    });
}
