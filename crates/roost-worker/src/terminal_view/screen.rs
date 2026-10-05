//! The terminal view owner's screen side: which transport each registry socket
//! answers on, one `CellSink` per LOCAL socket (`local:<socketId>`), and the
//! watch set deciding which sessions that sink forwards. A coordinator-relayed
//! socket gets no sink: its cells ride the single `coord` sink and the
//! coordinator seeds it from its own replica. Ports
//! `apps/worker/src/terminal/view/terminal-view-owner-screen.ts`.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use roost_proto::{PbCellGridFrame, TerminalViewStateFrame, WTerminalViewState};
use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::wire::brand::{ChannelId, SessionId};
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;

use crate::session::cell_sink::{CellSink, CellSinkResult, FrameTimings, local_cell_sink_id};
use crate::uplink::Uplink;

use super::session_port::ViewSessionPort;

/// A browser on this machine: it takes both the view decision and the cells.
///
/// Implemented by the local terminal door (W-DOOR). Every method is called with
/// a view-owner or emitter lock held, so an implementation only enqueues and
/// never calls back into the owner synchronously.
pub trait LocalViewTransport: Send + Sync + std::fmt::Debug {
    /// One view decision for this socket.
    fn send_view_state(&self, frame: TerminalViewStateFrame);
    /// One cell frame of a session this socket watches, with its one wire
    /// conversion (session id empty; the transport names the session).
    fn send_cell_frame(
        &self,
        channel_id: ChannelId,
        frame: &CellGridFrame,
        wire: &PbCellGridFrame,
    ) -> CellSinkResult;
    /// One part of a parked full of a session this socket watches.
    fn send_snapshot_part(
        &self,
        channel_id: ChannelId,
        part: &CellGridSnapshotPart,
        timings: FrameTimings,
    ) -> CellSinkResult;
    /// The sink registry dropped this socket for a delivery overflow.
    fn on_overflow(&self);
    /// A live view's lease ran out, so this socket stopped heartbeating.
    fn on_view_expired(&self);
}

/// The grant's session scope for a local socket (v2 `allowsSession`).
pub type SessionScope = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// Where one registry socket's view decisions land.
#[derive(Clone)]
pub(super) enum ViewTransport {
    Local {
        transport: Arc<dyn LocalViewTransport>,
        scope: SessionScope,
    },
    /// A browser on another machine, reached by relaying the decision upstream.
    /// The coordinator authorized it for the session before relaying.
    Coordinator,
}

struct OwnedSocket {
    transport: ViewTransport,
    /// Sessions this socket paints; shared with its sink, which forwards
    /// nothing else.
    watching: Arc<Mutex<BTreeSet<SessionId>>>,
    /// The last stream this socket was seeded on, per session, so entering a
    /// fresh stream is distinguishable from renewing inside the current one.
    streams: HashMap<SessionId, String>,
}

/// Every socket the registry knows, by id.
#[derive(Default)]
pub(super) struct ScreenSockets {
    sockets: HashMap<String, OwnedSocket>,
}

impl ScreenSockets {
    /// Bind the transport before the registry admits the socket.
    pub(super) fn attach(&mut self, socket_id: &str, transport: ViewTransport) {
        self.sockets.insert(
            socket_id.to_owned(),
            OwnedSocket {
                transport,
                watching: Arc::default(),
                streams: HashMap::new(),
            },
        );
    }

    pub(super) fn has(&self, socket_id: &str) -> bool {
        self.sockets.contains_key(socket_id)
    }

    /// v2's `allowsSession` for this socket: a relayed socket allows every
    /// session, a local one follows its grant.
    pub(super) fn allows(&self, socket_id: &str, session_id: &str) -> bool {
        match self.sockets.get(socket_id).map(|socket| &socket.transport) {
            Some(ViewTransport::Local { scope, .. }) => scope(session_id),
            Some(ViewTransport::Coordinator) => true,
            None => false,
        }
    }

    pub(super) fn is_local(&self, socket_id: &str) -> bool {
        self.local_transport(socket_id).is_some()
    }

    pub(super) fn local_transport(&self, socket_id: &str) -> Option<Arc<dyn LocalViewTransport>> {
        match self.sockets.get(socket_id).map(|socket| &socket.transport) {
            Some(ViewTransport::Local { transport, .. }) => Some(Arc::clone(transport)),
            _ => None,
        }
    }

    /// The transport kind, for the one log line a lease expiry writes.
    pub(super) fn kind(&self, socket_id: &str) -> &'static str {
        match self.sockets.get(socket_id).map(|socket| &socket.transport) {
            Some(ViewTransport::Local { .. }) => "local",
            Some(ViewTransport::Coordinator) => "coordinator",
            None => "unknown",
        }
    }

    pub(super) fn coordinator_socket_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .sockets
            .iter()
            .filter(|(_, socket)| matches!(socket.transport, ViewTransport::Coordinator))
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort();
        ids
    }

    /// v2 `registerSocket`: the cell sink a LOCAL socket paints through, for
    /// the caller to register once no owner lock is held.
    pub(super) fn cell_sink(
        &self,
        socket_id: &str,
        sessions: &Arc<dyn ViewSessionPort>,
    ) -> Option<Arc<dyn CellSink>> {
        let socket = self.sockets.get(socket_id)?;
        let ViewTransport::Local { transport, .. } = &socket.transport else {
            return None;
        };
        Some(Arc::new(LocalCellSink {
            id: local_cell_sink_id(socket_id),
            transport: Arc::clone(transport),
            watching: Arc::clone(&socket.watching),
            sessions: Arc::clone(sessions),
        }))
    }

    /// v2 `unregisterSocket`: forget the socket, returning the local sink id
    /// the caller must unregister.
    pub(super) fn detach(&mut self, socket_id: &str) -> Option<String> {
        let socket = self.sockets.remove(socket_id)?;
        matches!(socket.transport, ViewTransport::Local { .. })
            .then(|| local_cell_sink_id(socket_id))
    }

    /// Forget every socket, returning every local sink id to unregister.
    pub(super) fn detach_all(&mut self) -> Vec<String> {
        let ids: Vec<String> = self.sockets.keys().cloned().collect();
        ids.iter().filter_map(|id| self.detach(id)).collect()
    }

    pub(super) fn set_watching(&mut self, socket_id: &str, session_id: &SessionId, watching: bool) {
        let Some(socket) = self.sockets.get_mut(socket_id) else {
            return;
        };
        let mut watched = socket
            .watching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if watching {
            watched.insert(session_id.clone());
            return;
        }
        watched.remove(session_id);
        drop(watched);
        socket.streams.remove(session_id);
    }

    /// The stream a socket was last seeded on for this session.
    pub(super) fn painting(&self, socket_id: &str, session_id: &SessionId) -> Option<&str> {
        self.sockets
            .get(socket_id)?
            .streams
            .get(session_id)
            .map(String::as_str)
    }

    /// Record the stream a full was just requested on for this socket.
    pub(super) fn note_stream(
        &mut self,
        socket_id: &str,
        session_id: &SessionId,
        stream_id: String,
    ) {
        if let Some(socket) = self.sockets.get_mut(socket_id) {
            socket.streams.insert(session_id.clone(), stream_id);
        }
    }

    /// Deliver one view decision to the transport that declared the view: a
    /// local socket directly, a relayed one as `terminal-view-state` upstream.
    pub(super) fn send_view_state(
        &self,
        socket_id: &str,
        frame: TerminalViewStateFrame,
        uplink: &Uplink,
    ) {
        match self.sockets.get(socket_id).map(|socket| &socket.transport) {
            Some(ViewTransport::Local { transport, .. }) => transport.send_view_state(frame),
            Some(ViewTransport::Coordinator) => {
                let sent =
                    uplink.send(CoordWorkerUpstream::TerminalViewState(WTerminalViewState {
                        socket_id: socket_id.to_owned(),
                        frame: roost_proto::buffa::MessageField::some(frame),
                        __buffa_unknown_fields: Default::default(),
                    }));
                if !sent {
                    tracing::debug!(
                        socket_id,
                        "a relayed terminal view state was not admitted to the link"
                    );
                }
            }
            None => {}
        }
    }
}

/// One local socket's cell sink.
struct LocalCellSink {
    id: String,
    transport: Arc<dyn LocalViewTransport>,
    watching: Arc<Mutex<BTreeSet<SessionId>>>,
    sessions: Arc<dyn ViewSessionPort>,
}

impl LocalCellSink {
    fn watches(&self, channel_id: ChannelId) -> bool {
        let watched = self
            .watching
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        watched
            .iter()
            .any(|session_id| self.sessions.channel_of(session_id) == Some(channel_id))
    }
}

impl CellSink for LocalCellSink {
    fn id(&self) -> &str {
        &self.id
    }

    // A channel this socket does not watch owes it nothing, so the frame is
    // reported delivered: answering "dropped" would latch a permanent repair
    // loop on every session the browser never opened.
    fn send_frame(
        &self,
        channel_id: ChannelId,
        frame: &CellGridFrame,
        wire: &PbCellGridFrame,
    ) -> CellSinkResult {
        if !self.watches(channel_id) {
            return CellSinkResult::Sent;
        }
        self.transport.send_cell_frame(channel_id, frame, wire)
    }

    fn send_snapshot_part(
        &self,
        channel_id: ChannelId,
        part: &CellGridSnapshotPart,
        timings: FrameTimings,
    ) -> CellSinkResult {
        if !self.watches(channel_id) {
            return CellSinkResult::Sent;
        }
        self.transport.send_snapshot_part(channel_id, part, timings)
    }

    fn on_overflow(&self) {
        self.transport.on_overflow();
    }
}
