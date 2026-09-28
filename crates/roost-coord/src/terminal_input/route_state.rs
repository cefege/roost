//! The record shapes of input-route control correlation: one bounded slot per
//! browser nonce, and the pending worker control bound into it. Shapes only --
//! every map that holds them, and every teardown, is
//! `terminal_input::route_results`, so socket and worker cleanup stay in one owner.
//! Ports `apps/coord/src/terminal/input/terminal-input-route-result-state.ts`.

use std::sync::Arc;

use roost_protocol::wire::WorkerFp;

use crate::coord_core::worker_handle::WorkerHandle;

/// A slot's identity. A slot is compared by identity, never by its nonce: a
/// socket that reuses a nonce after release must not be mistaken for the
/// admission it replaced.
pub type RouteControlSlotId = u64;

/// Which control a pending entry carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteControlKind {
    /// A route claim; settled only by a route result.
    Claim,
    /// A transport probe; settled only by a probe result.
    Probe,
}

/// One admitted browser control, reserved before any asynchronous lookup.
#[derive(Debug)]
pub struct RouteControlSlot {
    /// The Sync socket that reserved it.
    pub connection_id: String,
    /// The browser's nonce, restored on the typed result.
    pub browser_request_id: String,
    /// The worker control bound into it, once there is one.
    pub pending: Option<PendingRouteControl>,
}

/// One worker control, bound to the exact worker generation it was sent to.
#[derive(Debug, Clone)]
pub struct PendingRouteControl {
    /// Which control this is.
    pub kind: RouteControlKind,
    /// Identity of this binding, so a later binding of the same slot is never
    /// mistaken for this one.
    pub binding: u64,
    /// The worker generation the control went to.
    pub worker: Arc<WorkerHandle>,
    /// That generation's fingerprint.
    pub worker_fp: WorkerFp,
    /// The worker process epoch the control addressed.
    pub worker_epoch: String,
    /// That generation's connection identity.
    pub connection_generation: String,
    /// The claimed session; `None` for a probe.
    pub session_id: Option<String>,
    /// The claimed revision; `None` for a probe.
    pub revision: Option<u64>,
    /// The coordinator's own correlation id, once allocated.
    pub outer_request_id: Option<String>,
}

/// The key that makes a browser nonce unique within its socket.
#[must_use]
pub fn connection_nonce_key(connection_id: &str, request_id: &str) -> (String, String) {
    (connection_id.to_owned(), request_id.to_owned())
}
