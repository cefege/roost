//! The direct terminal frame owner for loopback and authenticated WebRTC packet
//! ports: one registry of ports, Hello admission against the grant store, live
//! session authorization through asynchronous input and history work, and the
//! three carrier lanes. The loopback door calls `on_open`; the peer owner calls
//! `open_peer_port` before it applies remote SDP, so neither races an ingress
//! frame. Ports `apps/worker/src/local-door/local-terminal-socket.ts`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use roost_proto::__buffa::oneof::local_terminal_client_frame::Frame as ClientFrame;
use roost_proto::__buffa::oneof::local_terminal_server_frame::Frame as ServerFrame;
use roost_proto::buffa::Message;
use roost_proto::{LocalTerminalClientFrame, LocalTerminalClosed};
use roost_protocol::terminal_peer::peer::TerminalPeerPacketLane;
use roost_protocol::wire::brand::ChannelId;
use tokio::runtime::Handle;

use super::authority::{AuthorizationDeps, Carrier, PortRegistry, PortSession, is_direct_port_session_authorized};
use super::controls::PortControls;
use super::delivery::send_local_terminal_frame;
use super::grant_scope::{GrantChange, GrantRemovalReason};
use super::grants::LocalTerminalGrantStore;
use super::port::{ExpectedPeer, PacketSendResult, PeerTerminalPacketPort, TerminalPacketPort};
use crate::local_door::LocalTerminalPreHelloOwner;
use crate::session::lifecycle::SessionManager;
use crate::session::table::SessionTable;
use crate::terminal_input::{TerminalInputRouteOwner, TerminalInputWorkBudget};
use crate::terminal_view::TerminalViewOwner;

/// What the socket owner is built from. v2 `LocalTerminalSocketDeps`.
#[derive(Debug, Clone)]
pub struct LocalTerminalSocketsDeps {
    pub manager: Arc<SessionManager>,
    pub sessions: Arc<SessionTable>,
    pub grants: LocalTerminalGrantStore,
    pub view: Arc<TerminalViewOwner>,
    pub work_budget: TerminalInputWorkBudget,
    pub routes: TerminalInputRouteOwner,
    pub worker_fingerprint: String,
    pub worker_epoch: String,
    /// Where deferred closes, input writes and history reads run.
    pub runtime: Handle,
}

/// What a peer carrier forwards its authenticated ingress to.
#[derive(Debug)]
pub struct PeerIngress {
    sockets: Weak<LocalTerminalSockets>,
    port: Arc<dyn TerminalPacketPort>,
}

impl PeerIngress {
    pub fn on_message(&self, bytes: &[u8]) {
        if let Some(sockets) = self.sockets.upgrade() {
            sockets.on_message(self.port.as_ref(), bytes);
        }
    }

    pub fn on_close(&self) {
        if let Some(sockets) = self.sockets.upgrade() {
            sockets.on_close(self.port.as_ref());
        }
    }
}

/// v2 `LocalTerminalSockets`.
#[derive(Debug)]
pub struct LocalTerminalSockets {
    pub(super) self_handle: Weak<Self>,
    pub(super) authorization: Arc<AuthorizationDeps>,
    pub(super) manager: Arc<SessionManager>,
    pub(super) view: Arc<TerminalViewOwner>,
    pub(super) work_budget: TerminalInputWorkBudget,
    pub(super) worker_fingerprint: String,
    pub(super) runtime: Handle,
    pub(super) pre_hello: LocalTerminalPreHelloOwner,
    pub(super) controls: PortControls,
    pub(super) generations: AtomicU64,
    disposed: AtomicBool,
    subscription: Mutex<Option<u64>>,
}

impl LocalTerminalSockets {
    /// Build the owner and subscribe it to grant changes.
    #[must_use]
    pub fn new(deps: LocalTerminalSocketsDeps) -> Arc<Self> {
        let owner = Arc::new_cyclic(|self_handle: &Weak<Self>| {
            let timeouts = self_handle.clone();
            let pre_hello = LocalTerminalPreHelloOwner::new(
                Arc::new(move |socket_id: &str| {
                    if let Some(sockets) = timeouts.upgrade() {
                        sockets.on_hello_timeout(socket_id);
                    }
                }),
                deps.runtime.clone(),
            );
            Self {
                self_handle: self_handle.clone(),
                authorization: Arc::new(AuthorizationDeps {
                    sessions: deps.sessions,
                    grants: deps.grants,
                    routes: deps.routes,
                    worker_epoch: deps.worker_epoch,
                    ports: PortRegistry::default(),
                }),
                manager: deps.manager,
                view: deps.view,
                work_budget: deps.work_budget,
                worker_fingerprint: deps.worker_fingerprint,
                runtime: deps.runtime,
                pre_hello,
                controls: PortControls::default(),
                generations: AtomicU64::new(0),
                disposed: AtomicBool::new(false),
                subscription: Mutex::new(None),
            }
        });
        let listener = Arc::downgrade(&owner);
        let subscription = owner.authorization.grants.subscribe(Arc::new(move |change: &GrantChange| {
            if let Some(sockets) = listener.upgrade() {
                sockets.on_grant_change(change);
            }
        }));
        *owner.subscription.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(subscription);
        owner
    }

    /// A loopback port opened: it has the Hello deadline to authenticate.
    pub fn on_open(&self, port: Arc<dyn TerminalPacketPort>) {
        self.register_port(Carrier::Loopback(port));
    }

    /// A peer port whose offer authorized `expected`. Its ingress goes through
    /// the returned handle.
    pub fn open_peer_port(&self, port: Arc<dyn PeerTerminalPacketPort>, expected: ExpectedPeer) -> PeerIngress {
        let ingress = PeerIngress { sockets: self.self_handle.clone(), port: Arc::clone(&port) as Arc<dyn TerminalPacketPort> };
        self.register_port(Carrier::Peer { port, expected });
        ingress
    }

    /// One binary frame from a carrier, in receive order.
    pub fn on_message(&self, port: &dyn TerminalPacketPort, bytes: &[u8]) {
        let Some(session) = self.authorization.ports.get(port.socket_id()) else {
            return;
        };
        if self.disposed.load(Ordering::Acquire) {
            return;
        }
        let Ok(frame) = LocalTerminalClientFrame::decode_from_slice(bytes) else {
            self.close(&session, "undecodable frame");
            return;
        };
        let Some(frame) = frame.frame else {
            return;
        };
        match frame {
            ClientFrame::Hello(hello) => self.accept(&session, &hello),
            _ if !session.is_authenticated() => self.close(&session, "hello required"),
            ClientFrame::TerminalView(command) => self.view.handle_view_command(session.socket_id(), &command),
            ClientFrame::TerminalResync(command) => self.view.handle_resync(session.socket_id(), &command),
            ClientFrame::Input(command) => self.start_input(&session, *command),
            ClientFrame::Scrollback(request) => self.start_scrollback(&session, *request),
            ClientFrame::InputRouteClaim(claim) => self.start_claim(&session, *claim),
            ClientFrame::TransportProbe(probe) => self.probe(&session, &probe),
        }
    }

    /// A carrier closed. A port the owner already retired is a no-op.
    pub fn on_close(&self, port: &dyn TerminalPacketPort) {
        let socket_id = port.socket_id();
        let Some(session) = self.authorization.ports.get(socket_id) else {
            return;
        };
        self.pre_hello.retire(socket_id);
        self.authorization.ports.remove(socket_id);
        self.controls.retire_port(socket_id);
        self.authorization.routes.retire_connection(socket_id);
        if session.is_authenticated() {
            self.view.close_socket(socket_id);
        }
        tracing::debug!(socket_id, "local terminal port retired");
    }

    /// v2 `revokeDevice`: the routes first, then the grants, whose removal
    /// closes every port the device held.
    pub fn revoke_device(&self, device_fingerprint: &str) {
        self.authorization.routes.revoke_device(device_fingerprint);
        self.authorization.grants.revoke_device(device_fingerprint);
    }

    /// Close every port and stop accepting new ones.
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::AcqRel) {
            return;
        }
        let subscription = self.subscription.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(subscription) = subscription {
            self.authorization.grants.unsubscribe(subscription);
        }
        for session in self.authorization.ports.all() {
            self.close(&session, "local terminal worker is stopping");
        }
        self.pre_hello.dispose();
        tracing::info!("the local terminal sockets were disposed");
    }

    fn register_port(&self, carrier: Carrier) {
        let loopback = matches!(carrier, Carrier::Loopback(_));
        let session = PortSession::new(carrier);
        let socket_id = session.socket_id().to_owned();
        if self.disposed.load(Ordering::Acquire) || self.authorization.ports.get(&socket_id).is_some() {
            session.port().close(1008, "local terminal port unavailable");
            return;
        }
        if loopback && !self.pre_hello.admit(&socket_id) {
            session.port().close(1008, "local terminal pre-hello capacity reached");
            return;
        }
        let port = Arc::clone(&session);
        if !self.authorization.ports.insert(session) {
            self.pre_hello.retire(&socket_id);
            port.port().close(1008, "local terminal port unavailable");
            return;
        }
        tracing::debug!(socket_id, loopback, "local terminal port registered");
    }

    fn on_hello_timeout(&self, socket_id: &str) {
        let Some(session) = self.authorization.ports.get(socket_id) else {
            return;
        };
        if !session.is_authenticated() {
            self.close(&session, "local terminal hello timed out");
        }
    }

    /// v2 `onGrantChange`. A lazy expiry can report from inside another owner's
    /// lock, so every close here is deferred; the port is fenced immediately.
    fn on_grant_change(&self, change: &GrantChange) {
        let (grant_id, reason) = match change {
            GrantChange::Removed { grant, reason: GrantRemovalReason::Expired } => (&grant.grant_id, "local terminal grant expired"),
            GrantChange::Removed { grant, .. } => (&grant.grant_id, "local terminal grant revoked"),
            GrantChange::Renewed { grant, removed_session_ids } if !removed_session_ids.is_empty() => {
                (&grant.grant_id, "local terminal grant scope reduced")
            }
            GrantChange::Installed { .. } | GrantChange::Renewed { .. } => return,
        };
        for session in self.authorization.ports.all() {
            if session.grant_id_is(grant_id) {
                self.close_deferred(&session, reason);
            }
        }
    }

    pub(super) fn is_session_authorized(&self, session: &Arc<PortSession>, session_id: &str) -> bool {
        is_direct_port_session_authorized(&self.authorization, session, session_id)
    }

    /// The session a keeper channel carries, for a direct cell frame.
    pub(super) fn session_of_channel(&self, channel_id: ChannelId) -> Option<String> {
        let channel = u16::try_from(channel_id.as_u32()).ok()?;
        self.authorization.sessions.session_of_channel(channel).map(|session_id| session_id.as_str().to_owned())
    }

    /// A control-lane frame; a refusal closes the port. False when refused.
    pub(super) fn send_control(&self, session: &Arc<PortSession>, frame: ServerFrame) -> bool {
        self.send_frame(session, frame, TerminalPeerPacketLane::Control, true) != PacketSendResult::Refused
    }

    pub(super) fn send_frame(&self, session: &Arc<PortSession>, frame: ServerFrame, lane: TerminalPeerPacketLane, close_on_refusal: bool) -> PacketSendResult {
        let result = send_local_terminal_frame(session.port(), frame, lane);
        if result == PacketSendResult::Refused && close_on_refusal {
            self.close(session, "local control delivery refused");
        }
        result
    }

    /// Close now. Callers hold no other owner's lock.
    pub(super) fn close(&self, session: &Arc<PortSession>, reason: &str) {
        if session.begin_close() {
            self.finish_close(session, reason);
        }
    }

    /// Fence the port now and tear it down on its own task: for callers inside
    /// the view owner's, the emitter's or the route owner's lock.
    pub(super) fn close_deferred(&self, session: &Arc<PortSession>, reason: &'static str) {
        if !session.begin_close() {
            return;
        }
        let sockets = self.self_handle.clone();
        let session = Arc::clone(session);
        self.runtime.spawn(async move {
            if let Some(sockets) = sockets.upgrade() {
                sockets.finish_close(&session, reason);
            }
        });
    }

    fn finish_close(&self, session: &Arc<PortSession>, reason: &str) {
        tracing::info!(socket_id = session.socket_id(), reason, "local terminal socket closing");
        let closed = LocalTerminalClosed { reason: reason.to_owned(), ..Default::default() };
        self.send_frame(session, ServerFrame::from(closed), TerminalPeerPacketLane::Control, false);
        self.on_close(session.port());
        session.port().close(1000, reason);
    }
}
