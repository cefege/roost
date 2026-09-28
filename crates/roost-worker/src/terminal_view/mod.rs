//! The worker's terminal view owner: membership, geometry aggregation and stream
//! generations for this worker's OWN sessions over the shared
//! `roost_protocol::terminal_view::ViewRegistry`, answering each decision on the
//! transport that declared the view and publishing the per-session projection.
//! The link's downstream dispatcher reaches it as `link_ports::TerminalViewPort`.
//! Ports `apps/worker/src/terminal/view/terminal-view-owner.ts`.

mod deferred;
mod replies;
mod screen;
mod session_port;
mod state;
mod streams;

use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use roost_observability::clock::EventClock;
use roost_proto::__buffa::oneof::d_terminal_view_relay::Command as RelayCommand;
use roost_proto::{DTerminalViewRelay, TerminalResyncCommand, TerminalViewCommand};
use roost_protocol::terminal_view::{NoTerminalViewSink, SocketRegistration};
use roost_protocol::viewport::TERMINAL_VIEW_SWEEP_MS;
use roost_protocol::wire::brand::SessionId;
use tokio::runtime::Handle;
use tokio::task::AbortHandle;

use crate::link_ports::TerminalViewPort;
use crate::uplink::Uplink;

use deferred::{Deferred, Work};
use screen::ViewTransport;
use state::OwnerState;

pub use screen::{LocalViewTransport, SessionScope};
pub use session_port::{SessionViewPort, ViewSessionPort};

/// What the owner is built from.
#[derive(Debug, Clone)]
pub struct TerminalViewOwnerDeps {
    pub sessions: Arc<dyn ViewSessionPort>,
    /// Where relayed view states and projections go upstream.
    pub uplink: Uplink,
    /// The registry's lease and park clock: `mono_ns` in milliseconds.
    pub clock: Arc<dyn EventClock>,
    /// Where stream applies, projection flushes and the sweep run.
    pub runtime: Handle,
}

/// A verified local terminal socket (built by the local door, W-DOOR).
pub struct LocalViewRegistration {
    pub socket_id: String,
    pub device_fingerprint: String,
    pub tab_id: String,
    /// The grant's session set. Nothing outside it becomes membership.
    pub allows_session: SessionScope,
    pub transport: Arc<dyn LocalViewTransport>,
}

impl std::fmt::Debug for LocalViewRegistration {
    /// The scope is a predicate and the transport a socket: a log line wants
    /// which socket, device and tab this is.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalViewRegistration")
            .field("socket_id", &self.socket_id)
            .field("device_fingerprint", &self.device_fingerprint)
            .field("tab_id", &self.tab_id)
            .finish_non_exhaustive()
    }
}

/// The worker's terminal view owner.
pub struct TerminalViewOwner {
    sessions: Arc<dyn ViewSessionPort>,
    clock: Arc<dyn EventClock>,
    runtime: Handle,
    state: Mutex<OwnerState>,
    self_handle: Weak<Self>,
    sweep: Mutex<Option<AbortHandle>>,
}

impl std::fmt::Debug for TerminalViewOwner {
    /// Counts, and never a blocking lock: a log line that formats the owner
    /// while one of its own decisions holds the state must not deadlock it.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut debug = formatter.debug_struct("TerminalViewOwner");
        if let Ok(state) = self.state.try_lock() {
            debug
                .field("registry", &state.registry)
                .field("streams", &state.streams.len());
        }
        debug.finish_non_exhaustive()
    }
}

impl TerminalViewOwner {
    /// Build the owner and start its lease/park sweep every
    /// `TERMINAL_VIEW_SWEEP_MS`. The sweep holds only a weak handle.
    #[must_use]
    pub fn new(deps: TerminalViewOwnerDeps) -> Arc<Self> {
        let owner = Arc::new_cyclic(|self_handle| Self {
            sessions: deps.sessions,
            clock: deps.clock,
            runtime: deps.runtime,
            state: Mutex::new(OwnerState::new(deps.uplink)),
            self_handle: self_handle.clone(),
            sweep: Mutex::new(None),
        });
        let weak = Arc::downgrade(&owner);
        let sweep = owner.runtime.spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(TERMINAL_VIEW_SWEEP_MS));
            loop {
                ticker.tick().await;
                let Some(owner) = weak.upgrade() else {
                    return;
                };
                owner.sweep_now();
            }
        });
        *owner
            .sweep
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(sweep.abort_handle());
        owner
    }

    /// A verified local terminal socket. The viewer key is
    /// `${fingerprint}:${tabId}` exactly as a relayed socket's is, so one tab
    /// reclaiming its own view across transports stays one identity. The
    /// production caller is the local terminal door (W-DOOR).
    pub fn register_local(&self, registration: LocalViewRegistration) {
        let now_ms = self.now_ms();
        let mut work = Work::new();
        {
            let mut state = self.locked();
            let socket_id = registration.socket_id.as_str();
            state.close_socket(socket_id, now_ms, &mut work);
            state.screen.attach(
                socket_id,
                ViewTransport::Local {
                    transport: registration.transport,
                    scope: registration.allows_session,
                },
            );
            let viewer_key = format!(
                "{}:{}",
                registration.device_fingerprint, registration.tab_id
            );
            register_socket(
                &mut state,
                socket_id,
                viewer_key,
                &registration.device_fingerprint,
                now_ms,
            );
            if let Some(sink) = state.screen.cell_sink(socket_id, &self.sessions) {
                work.push(Deferred::RegisterSink(sink));
            }
            tracing::info!(
                socket_id,
                device_fingerprint = %registration.device_fingerprint,
                tab_id = %registration.tab_id,
                "a local terminal view socket registered"
            );
        }
        self.run(work);
    }

    /// One relayed command from a browser the coordinator authenticated. The
    /// membership decision is synchronous and the keeper work it triggers is
    /// governed by the worker's own control ceiling, so `budget_ms` has no
    /// waiter here to serve.
    pub fn handle_relay(&self, relay: DTerminalViewRelay) {
        let Some(command) = relay.command else {
            return;
        };
        if relay.socket_id.is_empty() || relay.viewer_key.is_empty() {
            return;
        }
        let now_ms = self.now_ms();
        let mut work = Work::new();
        {
            let mut state = self.locked();
            ensure_coordinator_socket(
                &mut state,
                &relay.socket_id,
                relay.viewer_key,
                &relay.device_fingerprint,
                now_ms,
            );
            match command {
                RelayCommand::View(view) => {
                    state.view_command(&relay.socket_id, &view, now_ms, &mut work)
                }
                RelayCommand::Resync(resync) => state.resync(&relay.socket_id, &resync, &mut work),
            }
        }
        self.run(work);
    }

    /// One view command from a registered local socket (W-DOOR).
    pub fn handle_view_command(&self, socket_id: &str, command: &TerminalViewCommand) {
        let now_ms = self.now_ms();
        let mut work = Work::new();
        self.locked()
            .view_command(socket_id, command, now_ms, &mut work);
        self.run(work);
    }

    /// One resync request from a registered local socket (W-DOOR).
    pub fn handle_resync(&self, socket_id: &str, command: &TerminalResyncCommand) {
        let mut work = Work::new();
        self.locked().resync(socket_id, command, &mut work);
        self.run(work);
    }

    /// Park this socket's views and drop its delivery. The park grace and the
    /// lease sweep decide when they stop constraining the PTY.
    pub fn close_socket(&self, socket_id: &str) {
        let now_ms = self.now_ms();
        let mut work = Work::new();
        self.locked().close_socket(socket_id, now_ms, &mut work);
        self.run(work);
    }

    /// A coordinator reconnect invalidates only the coordinator's own browser
    /// sockets: a local socket keeps its views, its lease and the live stream,
    /// so a coordinator bounce never blanks a local pane. Every projection is
    /// re-announced because the new coordinator generation has none.
    pub fn drop_coordinator_sockets(&self) {
        let now_ms = self.now_ms();
        let mut work = Work::new();
        {
            let mut state = self.locked();
            let socket_ids = state.screen.coordinator_socket_ids();
            for socket_id in &socket_ids {
                state.close_socket(socket_id, now_ms, &mut work);
            }
            let sessions: Vec<SessionId> = state.streams.keys().cloned().collect();
            for session_id in &sessions {
                state.mark_projection(session_id, &mut work);
            }
            tracing::info!(
                sockets = socket_ids.len(),
                "coordinator terminal view sockets dropped"
            );
        }
        self.run(work);
    }

    /// A session closed: its membership and its stream identity go, instead of
    /// holding a dead geometry (v2 `main.ts` sessionClosed hook).
    pub fn close_session(&self, session_id: &SessionId) {
        let mut work = Work::new();
        {
            let mut state = self.locked();
            let outcome = state.registry.close_session(session_id);
            state.deliver_calls(&outcome.calls, &mut work);
            state.close_stream(session_id, &mut work);
        }
        self.run(work);
    }

    /// One lease/park sweep tick. Production runs it on the interval `new`
    /// starts; tests drive it against an injected clock.
    pub fn sweep_now(&self) {
        let now_ms = self.now_ms();
        let mut work = Work::new();
        {
            let mut state = self.locked();
            let outcome = state.registry.sweep(now_ms);
            state.deliver_calls(&outcome.calls, &mut work);
            for session_id in &outcome.changed {
                state.recompute(session_id, now_ms, &mut work);
            }
        }
        self.run(work);
    }

    /// Stop the sweep and release every socket, sink and stream.
    pub fn dispose(&self) {
        if let Some(sweep) = self
            .sweep
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            sweep.abort();
        }
        let mut work = Work::new();
        {
            let mut state = self.locked();
            state.disposed = true;
            state.registry = roost_protocol::terminal_view::ViewRegistry::new();
            for sink_id in state.screen.detach_all() {
                work.push(Deferred::UnregisterSink(sink_id));
            }
            state.streams.clear();
            state.dirty_projections.clear();
        }
        tracing::info!("the terminal view owner was disposed");
        self.run(work);
    }

    pub(super) fn stream_is_current(&self, session_id: &SessionId, stream_id: &str) -> bool {
        self.locked().stream_is_current(session_id, stream_id)
    }

    fn now_ms(&self) -> u64 {
        self.clock.mono_ns() / 1_000_000
    }

    fn locked(&self) -> MutexGuard<'_, OwnerState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl TerminalViewPort for TerminalViewOwner {
    fn relay(&self, request: DTerminalViewRelay) {
        self.handle_relay(request);
    }

    fn close_socket(&self, socket_id: &str) {
        TerminalViewOwner::close_socket(self, socket_id);
    }

    fn drop_coordinator_sockets(&self) {
        TerminalViewOwner::drop_coordinator_sockets(self);
    }
}

/// Admit a relayed browser socket the first time the coordinator names it.
fn ensure_coordinator_socket(
    state: &mut OwnerState,
    socket_id: &str,
    viewer_key: String,
    device_fingerprint: &str,
    now_ms: u64,
) {
    if state.screen.has(socket_id) {
        return;
    }
    state.screen.attach(socket_id, ViewTransport::Coordinator);
    register_socket(state, socket_id, viewer_key, device_fingerprint, now_ms);
    tracing::info!(
        socket_id,
        device_fingerprint,
        "a coordinator terminal view socket registered"
    );
}

/// The registry half of a socket registration. The worker delivers through its
/// screen, so the registry's own sink is never read here; the session scope is
/// set per command from the screen's predicate.
fn register_socket(
    state: &mut OwnerState,
    socket_id: &str,
    viewer_key: String,
    fingerprint: &str,
    now_ms: u64,
) {
    state.registry.register_socket(
        &SocketRegistration {
            socket_id: socket_id.to_owned(),
            viewer_key: Some(viewer_key),
            caller_fingerprint: fingerprint.to_owned(),
            session_ids: std::collections::BTreeSet::new(),
            sink: Arc::new(NoTerminalViewSink),
        },
        now_ms,
    );
}
