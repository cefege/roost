//! The live worker set, and the three transitions that change it: a generation
//! claims the worker, a credential is fenced, a socket is retired.
//!
//! Ported from `apps/coord/src/workers/worker-registry.ts:66-101`. The map
//! itself is `coord_core::worker_handle::WorkerRegistry` -- shared state, not a
//! collaborator -- so this file owns the POLICY around it: what a worker
//! becoming routable publishes, and what losing it publishes.
//!
//! PUBLISHING IS WHOLE-SET, NEVER A DELTA. Every connect and every disconnect
//! republishes the complete routable set (`worker-registry.ts:99-101`), because
//! a per-fingerprint fold would need a subscriber that can drop one, and a
//! subscriber that missed one shows a machine as online until the next worker
//! list. A fleet is a handful of machines, so the whole set is cheap.

use std::sync::Arc;

use roost_protocol::wire::WorkerFp;

use crate::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use crate::events::bus_domains::Buses;
use crate::events::bus_messages::WorkerRoutableSet;

/// The fingerprints a frame can reach right now, in a stable order.
#[must_use]
pub fn list_routable_fps(registry: &WorkerRegistry) -> Vec<WorkerFp> {
    registry.routable_fps()
}

/// Publish the whole routable set, so a browser's online indicator tracks the
/// coordinator's actual socket membership rather than a stale worker list.
pub fn publish_routable(buses: &Buses, registry: &WorkerRegistry) {
    let fps = list_routable_fps(registry);
    tracing::debug!(count = fps.len(), "publishing the routable worker set");
    buses.worker_routable_bus.publish(WorkerRoutableSet { fps });
}

/// Claim a worker's socket as its current generation, and republish the set.
///
/// The replaced generation is fenced by `WorkerRegistry::insert`, so a reconnect
/// that arrives while the old socket is still open leaves exactly one generation
/// able to carry a frame. Publishing here rather than at the snapshot barrier is
/// v2's ordering: a claimed generation is not yet routable, so the set this
/// publishes is unchanged, and a worker that never finishes its handshake is
/// never announced as online.
pub fn claim_generation(buses: &Buses, registry: &WorkerRegistry, handle: Arc<WorkerHandle>) {
    tracing::info!(
        worker_fp = %handle.worker_fp,
        connection_generation = %handle.connection_generation,
        "a worker generation claimed its fingerprint"
    );
    registry.insert(handle);
    publish_routable(buses, registry);
}

/// Mark a generation ready after its exact snapshot committed, and republish.
///
/// Returns whether this call was the one that made the worker routable, so a
/// duplicate snapshot does not republish the set.
pub fn mark_generation_ready(
    buses: &Buses,
    registry: &WorkerRegistry,
    handle: &Arc<WorkerHandle>,
) -> bool {
    let became_routable = handle.mark_ready();
    if became_routable {
        tracing::info!(
            worker_fp = %handle.worker_fp,
            connection_generation = %handle.connection_generation,
            "a worker generation is routable"
        );
        publish_routable(buses, registry);
    }
    became_routable
}

/// Fence a worker's credential permanently and drop its socket, returning the
/// handle that was fenced so a caller can name the generation it revoked.
///
/// The order is v2's and it is load-bearing: mark revoked, then unregister
/// (`worker-registry.ts:69-85`). Publishing the routable set is deliberately NOT
/// part of it -- in v2 that publication is a separate best-effort cleanup the
/// delete handler runs, and folding it in here would make a bus failure part of
/// a credential revocation.
pub fn fence_worker_credential(
    registry: &WorkerRegistry,
    worker_fp: &WorkerFp,
) -> Option<Arc<WorkerHandle>> {
    let fenced = registry.fence(worker_fp);
    registry.retire(worker_fp);
    fenced
}

/// Drop a worker's socket without fencing a credential, and republish the set.
///
/// This is the disconnect path: the socket is gone, so the entry goes, and the
/// SPA must stop showing the machine as online without waiting for a worker list.
pub fn retire_generation(buses: &Buses, registry: &WorkerRegistry, worker_fp: &WorkerFp) {
    registry.retire(worker_fp);
    tracing::info!(%worker_fp, "a worker socket was retired");
    publish_routable(buses, registry);
}
