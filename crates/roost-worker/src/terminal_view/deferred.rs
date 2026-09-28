//! The terminal view owner's effects that must run with its lock RELEASED:
//! every session-layer call (stream applies, full requests, sink registration)
//! and every local-transport callback that may re-enter the owner. The locked
//! decision records them in order; [`TerminalViewOwner::run`] performs them.
//! Ports the asynchronous halves of `apps/worker/src/terminal/view/terminal-view-owner*.ts`.

use std::sync::Arc;
use std::time::Instant;

use roost_protocol::viewport::TerminalGeometry;
use roost_protocol::wire::brand::SessionId;

use crate::session::cell_sink::CellSink;
use crate::session::ids::mint_uuid;
use crate::session::terminal_state::{StreamIntent, WorkerStreamResult};

use super::TerminalViewOwner;
use super::screen::LocalViewTransport;
use super::session_port::ViewStreamBudget;

/// The ordered effects one locked decision owes.
pub(super) type Work = Vec<Deferred>;

/// One apply the streams half decided, carried to the session layer.
#[derive(Debug, Clone)]
pub(super) struct StreamApply {
    pub(super) session_id: SessionId,
    pub(super) stream_id: String,
    pub(super) incarnation: u64,
    pub(super) geometry: Option<TerminalGeometry>,
    pub(super) retry: u8,
}

/// One effect performed after the owner's lock is released.
pub(super) enum Deferred {
    /// Drive one stream desire through the control lane.
    Apply(StreamApply),
    /// v2 `seedSocket` for a local socket.
    Seed {
        socket_id: String,
        session_id: SessionId,
    },
    /// v2 `ensureSocketStream`, then `seedSocket` when the socket fell behind.
    EnsureStream {
        socket_id: String,
        session_id: SessionId,
    },
    /// v2 `resyncSocket`: every repair is a fresh authoritative full.
    Resync {
        socket_id: String,
        session_id: SessionId,
    },
    RegisterSink(Arc<dyn CellSink>),
    UnregisterSink(String),
    /// A local socket's live lease lapsed; its transport ends the socket.
    ViewExpired(Arc<dyn LocalViewTransport>),
    /// Publish every dirty projection on a later turn.
    FlushProjections,
}

impl TerminalViewOwner {
    /// Perform one decision's effects, in the order it recorded them.
    pub(super) fn run(&self, work: Work) {
        for effect in work {
            match effect {
                Deferred::Apply(apply) => self.start_apply(apply),
                Deferred::Seed {
                    socket_id,
                    session_id,
                } => self.seed_socket(&socket_id, &session_id),
                Deferred::EnsureStream {
                    socket_id,
                    session_id,
                } => {
                    if self.socket_behind_stream(&socket_id, &session_id) {
                        self.seed_socket(&socket_id, &session_id);
                    }
                }
                Deferred::Resync {
                    socket_id,
                    session_id,
                } => self.resync_socket(&socket_id, &session_id),
                Deferred::RegisterSink(sink) => self.sessions.register_cell_sink(sink),
                Deferred::UnregisterSink(sink_id) => self.sessions.unregister_cell_sink(&sink_id),
                Deferred::ViewExpired(transport) => transport.on_view_expired(),
                Deferred::FlushProjections => self.schedule_flush(),
            }
        }
    }

    /// Hand one desire to the session layer and settle it when it resolves.
    /// The call itself is synchronous: the stream every live view was just told
    /// about is handed over before anything else this owner decides.
    fn start_apply(&self, apply: StreamApply) {
        let request_id = mint_uuid().unwrap_or_else(|_| apply.stream_id.clone());
        let budget = ViewStreamBudget {
            owner: self.self_handle.clone(),
            session_id: apply.session_id.clone(),
            stream_id: apply.stream_id.clone(),
            started: Instant::now(),
        };
        let intent = StreamIntent {
            request_id,
            session_id: apply.session_id.clone(),
            stream_id: apply.stream_id.clone(),
            enabled: apply.geometry.is_some(),
            cols: apply.geometry.map_or(0, |geometry| geometry.cols),
            rows: apply.geometry.map_or(0, |geometry| geometry.rows),
            budget: Some(Arc::new(budget)),
        };
        let pending = self.sessions.apply_stream_state(intent);
        let owner = self.self_handle.clone();
        self.runtime.spawn(async move {
            let result = pending.await;
            if let Some(owner) = owner.upgrade() {
                owner.finish_apply(&apply, &result);
            }
        });
    }

    fn finish_apply(&self, apply: &StreamApply, result: &WorkerStreamResult) {
        let mut work = Work::new();
        self.locked().finish_apply(apply, result, &mut work);
        self.run(work);
    }

    /// v2 `seedSocket`/`requestFull`: a forced full on the session's current
    /// stream, going through the snapshot request so another sink's pending
    /// cursor cannot swallow it.
    fn seed_socket(&self, socket_id: &str, session_id: &SessionId) {
        // A relayed socket is seeded from the coordinator's own replica: the
        // one `coord` sink is shared by every remote viewer, so a full here is a
        // stream-wide re-baseline (FAILURE-INDEX "One viewer re-attaching costs
        // every coordinator viewer a second baseline").
        if !self.locked().screen.is_local(socket_id) {
            return;
        }
        let Some(stream_id) = self.sessions.current_stream_id(session_id) else {
            return;
        };
        self.sessions.request_snapshot(session_id, &stream_id);
        tracing::debug!(socket_id, %session_id, stream_id = %stream_id, "a local terminal view was seeded");
        self.locked()
            .screen
            .note_stream(socket_id, session_id, stream_id);
    }

    /// v2 `ensureSocketStream`: the local socket entered a stream it was not
    /// already painting, so it owes a baseline.
    fn socket_behind_stream(&self, socket_id: &str, session_id: &SessionId) -> bool {
        let Some(current) = self.sessions.current_stream_id(session_id) else {
            return false;
        };
        let state = self.locked();
        state.screen.is_local(socket_id)
            && state.screen.painting(socket_id, session_id) != Some(current.as_str())
    }

    /// v2 `resyncSocket`: the worker keeps no record of what it shipped to one
    /// socket, so every repair is a fresh full — which also carries a relayed
    /// socket's repair to the coordinator replica that owns it.
    fn resync_socket(&self, socket_id: &str, session_id: &SessionId) {
        if !self.locked().screen.has(socket_id) {
            return;
        }
        let Some(stream_id) = self.sessions.current_stream_id(session_id) else {
            return;
        };
        self.sessions.request_snapshot(session_id, &stream_id);
        tracing::debug!(socket_id, %session_id, stream_id = %stream_id, "a terminal view resync requested a full");
        self.locked()
            .screen
            .note_stream(socket_id, session_id, stream_id);
    }

    /// v2 queues the projection flush as a microtask; a spawned task runs after
    /// the decision that scheduled it has returned.
    fn schedule_flush(&self) {
        let owner = self.self_handle.clone();
        self.runtime.spawn(async move {
            if let Some(owner) = owner.upgrade() {
                let now_ms = owner.now_ms();
                owner.locked().flush_projections(now_ms);
            }
        });
    }
}
