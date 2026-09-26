//! The terminal view hub: which worker owns a session's views, and the
//! membership and geometry its owner-mode relay publishes.
//!
//! One field on `CoordServices`, reached as `core.services.views`, and handed
//! to the workers domain as the `TerminalViewLifecycle` seam. A respawn reads
//! the geometry the effective viewer set produced from here, so a second hub
//! would be a second geometry for one session.
//!
//! `new()` takes nothing and must keep taking nothing: the terminal memory
//! ceiling the hub sizes itself against is read at call time from
//! `core.services.boot`.
//!
//! WHAT THIS OWNS, AND WHAT IT DELIBERATELY DOES NOT. It owns WHO IS VIEWING:
//! the socket registry, the leases, the park grace, the one
//! [`record::view_constrains`] predicate, and the one aggregation
//! (`roost_protocol::viewport::minimum_terminal_geometry`). It does NOT own a
//! session's stream: an owner-mode worker mints the stream id and the effective
//! geometry it runs, and this hub relays browser decisions to it and adopts
//! what it published. That is the coordinator-owned authority fallback being
//! dropped -- v2's `TerminalViewStreamController`, with its snapshot requests,
//! redrive and unavailable policy, has no port here and no stub for it.
//!
//! WHY ONE MINIMIZER PER SESSION. A session either has an owner-mode worker, in
//! which case every browser decision is relayed and the worker's registry
//! minimizes; or it does not, in which case this hub's registry is the only
//! minimizer. Admitting local membership for an owned session would give one
//! PTY two geometries, which is `docs/FAILURE-INDEX.md`, "A session stays
//! clipped to a viewer that is no longer looking".

mod admit;
mod commands;
mod machine;
mod owner;
mod record;
mod registry;
mod relay;
mod settle;
mod sink;
mod tombstone;
mod worker_link;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

use roost_proto::{
    TerminalResyncCommand, TerminalViewCommand, TerminalViewStateFrame, TerminalViewStatus,
    WTerminalViewProjection,
};
use roost_protocol::viewport::{TERMINAL_VIEW_SWEEP_MS, TerminalGeometry};
use roost_protocol::wire::{SessionId, WorkerFp};

use crate::coord_core::seams::TerminalViewLifecycle;

pub use owner::{OwnerIndex, OwnerRegistration, OwnerRow, TERMINAL_VIEW_OWNER_CAPABILITY};
pub use record::{ViewInput, ViewStats};
pub use registry::{MembershipOutcome, SocketRegistration, ViewRegistry};
pub use relay::{NoOwnerViewTransport, OwnerRelay, OwnerViewTransport, RelayIdentity};
pub use sink::{NoTerminalViewSink, PendingReply, SinkCall, TerminalViewSink};
pub use worker_link::{TERMINAL_VIEW_RELAY_BUDGET_MS, WorkerLinkViewTransport};

/// The view state one coordinator process holds.
#[derive(Debug, Default)]
pub struct TerminalViewHub {
    pub(super) registry: std::sync::Mutex<ViewRegistry>,
    pub(super) owners: Arc<OwnerIndex>,
    pub(super) relay: OwnerRelay,
    /// The size each session's PTY runs at. A session keeps its last effective
    /// geometry when its last viewer leaves, so a flapping link cannot become a
    /// stream re-mint storm.
    pub(super) effective: std::sync::Mutex<std::collections::HashMap<SessionId, TerminalGeometry>>,
}

impl TerminalViewHub {
    /// A hub that has seen no view membership.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install the worker-link transport the owner relay writes through.
    pub fn set_owner_transport(&self, transport: Arc<dyn OwnerViewTransport>) {
        self.relay.set_transport(transport);
    }

    /// The owner index, for the worker-connect path that registers owners and
    /// binds sessions to them.
    #[must_use]
    pub fn owners(&self) -> &Arc<OwnerIndex> {
        &self.owners
    }

    /// Admit a Sync socket as a view holder.
    pub fn register_socket(&self, registration: &SocketRegistration, now_ms: u64) {
        self.locked().register_socket(registration, now_ms);
    }

    /// A Sync socket is gone: park its views, then tell every owner it reached.
    pub fn close_socket(&self, socket_id: &str, now_ms: u64) {
        self.locked().close_socket(socket_id, now_ms);
        self.relay.close_socket(socket_id);
    }

    /// A device lost its authority: its sockets stop reaching any owner before
    /// its records go, so no later terminal delta is delivered to it.
    pub fn remove_fingerprint(&self, fingerprint: &str, now_ms: u64) {
        let sockets = self.locked().socket_ids_for_fingerprint(fingerprint);
        for socket_id in &sockets {
            self.close_socket(socket_id, now_ms);
        }
        let outcome = self.locked().remove_fingerprint(fingerprint);
        self.settle(outcome, now_ms);
    }

    /// One view command from a Sync socket.
    ///
    /// An owned session is relayed; every other session is decided here.
    pub fn handle_view_command(&self, socket_id: &str, command: &TerminalViewCommand, now_ms: u64) {
        let Some(owner) = self.owner_of(command.session_id.as_str()) else {
            let outcome = self
                .locked()
                .machine()
                .handle_view_command(socket_id, command, now_ms);
            self.settle(outcome, now_ms);
            return;
        };
        let Some(socket) = self.locked().socket(socket_id).cloned() else {
            return;
        };
        if let Some(relayed) = self.relay.relay_view(&socket, &owner, command)
            && let Some(refusal) = relayed.refusal
        {
            self.deliver(std::slice::from_ref(&refusal), &[], now_ms);
        }
    }

    /// One resync request from a Sync socket.
    pub fn handle_resync(&self, socket_id: &str, command: &TerminalResyncCommand, now_ms: u64) {
        if let Some(owner) = self.owner_of(command.session_id.as_str()) {
            let Some(socket) = self.locked().socket(socket_id).cloned() else {
                return;
            };
            if self.relay.relay_resync(&socket, &owner, command) {
                return;
            }
            tracing::debug!(
                worker_fp = %owner,
                socket_id,
                session_id = %command.session_id,
                "an owner could not be reached for a resync; the next view state answers it"
            );
            return;
        }
        let outcome = self.locked().machine().handle_resync(socket_id, command);
        self.deliver(&[], &outcome.calls, now_ms);
    }

    /// One view decision from the session's owner.
    ///
    /// The screen expectation is installed BEFORE the browser sees the frame:
    /// a browser reacts to an accepted stream id by applying whatever cells
    /// arrive next, and a replica that has not been told to expect that stream
    /// would fold them against the previous baseline.
    pub fn apply_owner_view_state(
        &self,
        worker_fp: &WorkerFp,
        socket_id: &str,
        frame: &TerminalViewStateFrame,
    ) {
        let Ok(session_id) = SessionId::try_from(frame.session_id.clone()) else {
            tracing::warn!(
                worker_fp = %worker_fp,
                socket_id,
                session_id = %frame.session_id,
                "an owner view state named a session id that is not one"
            );
            return;
        };
        if self.owner_of(&frame.session_id).as_ref() != Some(worker_fp) {
            tracing::debug!(
                worker_fp = %worker_fp,
                %session_id,
                "a terminal view state arrived from a worker that does not own the session"
            );
            return;
        }
        let Some(socket) = self.locked().socket(socket_id).cloned() else {
            return;
        };
        if !socket.allows(&frame.session_id) {
            return;
        }
        // Only a membership reply carries a stream: a rejection or an inactive
        // acknowledgement leaves both empty and must not disturb the replica.
        let member = !frame.stream_id.is_empty();
        let previously_expected = socket.sink.expected_stream_id(&session_id);
        if member {
            socket.sink.expect_stream(
                &session_id,
                &frame.stream_id,
                frame.effective_cols,
                frame.effective_rows,
            );
        }
        let (watching, attached) = self
            .relay
            .track(socket_id, &session_id, &frame.view_id, member);
        socket.sink.set_watching(socket_id, &session_id, watching);
        // Only the decision that ATTACHES a socket may seed it: a lease
        // heartbeat re-declares the same view every few seconds, and seeding on
        // those would push a duplicate full on every beat.
        let seeded = attached && socket.sink.seed_socket(socket_id, &session_id);
        if attached && !seeded && previously_expected.as_deref() == Some(frame.stream_id.as_str()) {
            socket.sink.invalidate(
                &session_id,
                "owner view attached without a replica baseline",
            );
        }
        socket.sink.enqueue_terminal_state(
            socket_id,
            sink::view_state_frame(
                &frame.view_id,
                &frame.session_id,
                frame.revision,
                frame.active,
                &frame.stream_id,
                // A generated enum field arrives wrapped in `EnumValue<E>`,
                // which has no `Deref` and no `From<EnumValue<E>> for E`: the
                // only way back to the enum is `as_known`.
                //
                // NEVER default this to `Accepted` to make the field total. A
                // build that cannot read the owner's decision must not tell a
                // browser its view was admitted: the client acts on an accepted
                // status by applying whatever cells arrive next, so an
                // invented acceptance is a permission this build never
                // confirmed. `Unspecified` is the proto zero value, and it says
                // what is true — this build does not know — instead of what
                // would be convenient.
                frame.status.as_known().unwrap_or(TerminalViewStatus::Unspecified),
                frame.effective_cols,
                frame.effective_rows,
                &frame.reason,
            ),
            &frame.session_id,
        );
    }

    /// One published membership from the session's owner.
    pub fn apply_owner_projection(
        &self,
        worker_fp: &WorkerFp,
        projection: &WTerminalViewProjection,
    ) {
        self.owners.apply_projection(worker_fp, projection);
    }

    /// A worker advertised the owner capability on this connection.
    pub fn register_owner(self: &Arc<Self>, worker_fp: &WorkerFp) -> OwnerRegistration {
        self.owners.register_owner(worker_fp)
    }

    /// A worker's hello did not carry the owner capability.
    pub fn clear_owner(&self, worker_fp: &WorkerFp) {
        self.owners.clear_owner(worker_fp);
    }

    /// Route reconciliation bound these sessions to this worker.
    pub fn bind_sessions(&self, worker_fp: &WorkerFp, session_ids: &[SessionId]) {
        for session_id in session_ids {
            self.owners.bind_session(session_id, worker_fp);
        }
    }

    /// A session closed: its membership, its claims and its owner row go.
    pub fn close_session(&self, session_id: &SessionId, now_ms: u64) {
        let outcome = self.locked().close_session(session_id);
        self.owners.drop_projection(session_id);
        self.locked_effective().remove(session_id);
        self.settle(outcome, now_ms);
    }

    /// One sweep tick, for a caller that drives the sweep itself.
    pub fn sweep(&self, now_ms: u64) {
        let outcome = self.locked().sweep(now_ms);
        self.settle(outcome, now_ms);
    }

    /// The size a session's PTY runs at: the per-axis minimum of the viewers
    /// that currently constrain it, or `None` while nothing has ever watched it.
    #[must_use]
    pub fn session_geometry(
        &self,
        session_id: &SessionId,
        now_ms: u64,
    ) -> Option<TerminalGeometry> {
        let held = self.locked_effective().get(session_id).copied();
        match held {
            Some(geometry) => Some(geometry),
            None => self.recompute(session_id, now_ms),
        }
    }

    /// The per-viewer diagnostic rows behind a session's effective geometry.
    #[must_use]
    pub fn viewer_inputs(&self, session_id: &SessionId, now_ms: u64) -> Vec<ViewInput> {
        self.locked().viewer_inputs(session_id, now_ms)
    }

    /// How many of a session's records are live and how many are parked.
    #[must_use]
    pub fn view_stats(&self, session_id: &SessionId) -> ViewStats {
        self.locked().view_stats(session_id)
    }

    /// The devices holding a view of this session.
    #[must_use]
    pub fn active_viewer_fingerprints(&self, session_id: &SessionId) -> BTreeSet<String> {
        self.locked().active_fingerprints(session_id)
    }

    /// Every session's per-device viewer geometry, for presence.
    #[must_use]
    pub fn viewer_projection(&self) -> BTreeMap<SessionId, BTreeMap<String, TerminalGeometry>> {
        self.locked().viewer_projection()
    }

    /// The owner-mode worker that owns a session's views, if one does.
    #[must_use]
    pub fn owner_for_session(&self, session_id: &str) -> Option<WorkerFp> {
        let session_id = SessionId::try_from(session_id).ok()?;
        self.owners.owner_for_session(&session_id)
    }
}

impl TerminalViewLifecycle for TerminalViewHub {
    fn notify_worker_retired(&self, worker_fp: &WorkerFp, session_ids: &[SessionId]) {
        self.owners.drop_owner(worker_fp);
        let now_ms = crate::serve::now_ms().max(0) as u64;
        for session_id in session_ids {
            // The worker is gone, so nothing can answer for these sessions any
            // more: their membership is released rather than left to a lease
            // that will never be renewed.
            self.owners.drop_projection(session_id);
            self.locked_effective().remove(session_id);
            let outcome = self.locked().close_session(session_id);
            self.settle(outcome, now_ms);
        }
        tracing::info!(
            worker_fp = %worker_fp,
            sessions = session_ids.len(),
            "terminal views released for a retired worker"
        );
    }

    fn effective_geometry(&self, session_id: &SessionId) -> Option<TerminalGeometry> {
        let now_ms = crate::serve::now_ms().max(0) as u64;
        if let Some(row) = self.owners.row(session_id) {
            return row.effective;
        }
        self.session_geometry(session_id, now_ms)
    }
}

/// Sweep lapsed leases and lapsed park graces on an interval, for a booted
/// coordinator. The lead calls this from `serve` beside the other schedulers.
pub fn spawn_view_sweep(views: Arc<TerminalViewHub>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(TERMINAL_VIEW_SWEEP_MS));
        loop {
            ticker.tick().await;
            let now_ms = crate::serve::now_ms().max(0) as u64;
            views.sweep(now_ms);
        }
    });
}
