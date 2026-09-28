//! The terminal view owner's locked state and how one membership decision is
//! settled: run the shared machine, deliver its host effects, recompute what
//! moved, then hand each answer to `replies.rs`; plus the coalesced projection
//! publication. Ports the locked halves of
//! `apps/worker/src/terminal/view/terminal-view-owner.ts`; `mod.rs` calls it.

use std::collections::{BTreeMap, HashMap};

use roost_proto::{
    PbTerminalViewInput, TerminalViewCommand, TerminalViewStatus, WTerminalViewProjection,
};
use roost_protocol::terminal_view::{SinkCall, ViewRegistry};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::uplink::Uplink;

use super::deferred::{Deferred, Work};
use super::screen::ScreenSockets;
use super::streams::StreamSession;

/// Everything the owner's one lock guards.
pub(super) struct OwnerState {
    pub(super) uplink: Uplink,
    pub(super) registry: ViewRegistry,
    pub(super) streams: HashMap<SessionId, StreamSession>,
    pub(super) screen: ScreenSockets,
    /// Sessions whose projection changed since the last flush, in order.
    pub(super) dirty_projections: Vec<SessionId>,
    pub(super) flush_scheduled: bool,
    pub(super) disposed: bool,
    /// Distinguishes a stream entry from a later one for the same session, so
    /// an apply that outlived a close cannot settle the entry that replaced it.
    pub(super) incarnations: u64,
}

impl OwnerState {
    pub(super) fn new(uplink: Uplink) -> Self {
        Self {
            uplink,
            registry: ViewRegistry::new(),
            streams: HashMap::new(),
            screen: ScreenSockets::default(),
            dirty_projections: Vec::new(),
            flush_scheduled: false,
            disposed: false,
            incarnations: 0,
        }
    }

    /// One view command from one registered socket.
    pub(super) fn view_command(
        &mut self,
        socket_id: &str,
        command: &TerminalViewCommand,
        now_ms: u64,
        work: &mut Work,
    ) {
        let path = self.command_path(socket_id, command);
        let outcome = self.scoped(socket_id, &command.session_id, |registry| {
            registry
                .machine()
                .handle_view_command(socket_id, command, now_ms)
        });
        self.deliver_calls(&outcome.calls, work);
        let mut desired = BTreeMap::new();
        for session_id in &outcome.changed {
            let broadcast = self.recompute(session_id, now_ms, work);
            desired.insert(session_id.clone(), broadcast);
        }
        for reply in &outcome.replies {
            let session_id = SessionId::try_from(reply.session_id().to_owned()).ok();
            let broadcast = session_id
                .as_ref()
                .and_then(|id| desired.get(id))
                .copied()
                .unwrap_or(false);
            self.finish_reply(reply, path, broadcast, session_id, work);
        }
    }

    /// One resync request. v2 serves it only while the stream the client named
    /// is still this session's stream.
    pub(super) fn resync(
        &mut self,
        socket_id: &str,
        command: &roost_proto::TerminalResyncCommand,
        work: &mut Work,
    ) {
        let outcome = self.scoped(socket_id, &command.session_id, |registry| {
            registry.machine().handle_resync(socket_id, command)
        });
        let current = SessionId::try_from(command.session_id.clone())
            .ok()
            .and_then(|id| self.streams.get(&id))
            .is_some_and(|stream| stream.stream_id == command.stream_id);
        if current {
            self.deliver_calls(&outcome.calls, work);
        }
    }

    /// Run one machine call with v2's `allowsSession(sessionId)` predicate
    /// answered for exactly the session it names. The machine asks about no
    /// other session, and clearing the answer afterwards keeps the socket's
    /// allowed set from accumulating every session a browser ever named.
    fn scoped<T>(
        &mut self,
        socket_id: &str,
        session_id: &str,
        call: impl FnOnce(&mut ViewRegistry) -> T,
    ) -> T {
        let allowed = self.screen.allows(socket_id, session_id);
        self.registry
            .set_session_allowed(socket_id, session_id, allowed);
        let answer = call(&mut self.registry);
        self.registry
            .set_session_allowed(socket_id, session_id, false);
        answer
    }

    /// Park this socket's views and drop its delivery. Reports whether the
    /// socket was registered at all.
    pub(super) fn close_socket(&mut self, socket_id: &str, now_ms: u64, work: &mut Work) -> bool {
        if self.registry.socket(socket_id).is_none() {
            return false;
        }
        self.registry.close_socket(socket_id, now_ms);
        if let Some(sink_id) = self.screen.detach(socket_id) {
            work.push(Deferred::UnregisterSink(sink_id));
        }
        tracing::info!(
            socket_id,
            "a terminal view socket closed; its views are parked"
        );
        true
    }

    /// The host effects the membership machine decided.
    pub(super) fn deliver_calls(&mut self, calls: &[SinkCall], work: &mut Work) {
        for call in calls {
            match call {
                SinkCall::Watching {
                    socket_id,
                    session_id,
                    watching,
                } => self.screen.set_watching(socket_id, session_id, *watching),
                SinkCall::Resync {
                    socket_id,
                    session_id,
                    ..
                } => work.push(Deferred::Resync {
                    socket_id: socket_id.clone(),
                    session_id: session_id.clone(),
                }),
                SinkCall::LiveViewExpired {
                    socket_id,
                    view_id,
                    session_id,
                } => {
                    tracing::warn!(
                        socket_id = %socket_id,
                        view_id = %view_id,
                        %session_id,
                        transport = self.screen.kind(socket_id),
                        "a live terminal view's lease expired"
                    );
                    // The coordinator owns its own sockets' lifetime; a local
                    // one is ours to end.
                    if let Some(transport) = self.screen.local_transport(socket_id) {
                        work.push(Deferred::ViewExpired(transport));
                    }
                }
            }
        }
    }

    /// Answer every live view of a session (v2 `registry.broadcast`).
    pub(super) fn broadcast(
        &mut self,
        session_id: &SessionId,
        status: TerminalViewStatus,
        reason: &str,
    ) {
        for reply in self.registry.broadcast(session_id, status, reason) {
            self.reply_view(&reply);
        }
    }

    /// Republish this session's projection on the next flush.
    pub(super) fn mark_projection(&mut self, session_id: &SessionId, work: &mut Work) {
        if self.disposed {
            return;
        }
        if !self.dirty_projections.contains(session_id) {
            self.dirty_projections.push(session_id.clone());
        }
        if self.flush_scheduled {
            return;
        }
        self.flush_scheduled = true;
        work.push(Deferred::FlushProjections);
    }

    /// One frame per dirty session: a resize storm changes membership many
    /// times before the flush runs.
    pub(super) fn flush_projections(&mut self, now_ms: u64) {
        self.flush_scheduled = false;
        let sessions = std::mem::take(&mut self.dirty_projections);
        if self.disposed {
            return;
        }
        for session_id in sessions {
            let stream = self.streams.get(&session_id);
            let effective = stream.and_then(|stream| stream.effective);
            let viewers = self
                .registry
                .viewer_inputs(&session_id, now_ms)
                .into_iter()
                .map(|input| PbTerminalViewInput {
                    fingerprint: input.fingerprint,
                    view_id: input.view_id,
                    cols: input.cols,
                    rows: input.rows,
                    parked: input.parked,
                    constrains: input.constrains,
                    __buffa_unknown_fields: Default::default(),
                })
                .collect();
            let projection = WTerminalViewProjection {
                session_id: session_id.as_str().to_owned(),
                viewers,
                effective_cols: effective.map_or(0, |geometry| geometry.cols),
                effective_rows: effective.map_or(0, |geometry| geometry.rows),
                stream_id: stream
                    .map(|stream| stream.stream_id.clone())
                    .unwrap_or_default(),
                __buffa_unknown_fields: Default::default(),
            };
            if !self
                .uplink
                .send(CoordWorkerUpstream::TerminalViewProjection(projection))
            {
                tracing::debug!(%session_id, "a terminal view projection was not admitted to the link");
            }
        }
    }
}
