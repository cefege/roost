//! What a revoked key loses in this process the moment its revocation commits:
//! its pending event publications, its terminal views, and every open Sync
//! socket it holds, closed `4001 revoked`.
//!
//! Called by `DevicesRevoke`, `DevicesRotateCurrent` and `AuthLogout`
//! (`auth::rpc_devices`) right after the key cache is invalidated. Ports the
//! `onKeyRevoked` / `closeRevokedSockets` hook of `apps/coord/src/main.ts`.
//!
//! A socket already open verified its key once, at upgrade; nothing re-reads
//! it afterwards, so without this hook a revoked browser keeps its live feed
//! until it happens to reconnect. The open-time generation check in
//! `sync_ws::socket_open` covers the socket that is still opening.

use std::sync::PoisonError;

use crate::services::CoordServices;

/// Release everything live the revoked `fingerprint` holds.
pub fn release_revoked_key(services: &CoordServices, fingerprint: &str) {
    services
        .pending_publications
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear_worker(fingerprint);
    services
        .views
        .remove_fingerprint(fingerprint, crate::sync_ws::driver::now_ms());
    services
        .feed
        .open_sockets()
        .close_for_fingerprint(fingerprint);
    tracing::info!(event = "auth", action = "revoked_key_released", %fingerprint, "a revoked key's live state is released");
}
