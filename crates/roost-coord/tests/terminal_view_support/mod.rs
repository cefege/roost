//! The shared harness for the terminal view hub tests: a hub, sockets that
//! record every frame and host effect they were given, and an owner transport
//! that records every relay.
//!
//! Separate from the test files that use it because `tests/*.rs` are
//! independent crates, so a harness has to live in a module both can include.

use std::sync::Arc;

use roost_coord::terminal_view::{SocketRegistration, TerminalViewHub};
use roost_proto::{TerminalViewCommand, TerminalViewStateFrame, TerminalViewStatus};
use roost_protocol::wire::{SessionId, WorkerFp};

mod sink;

pub use sink::{Recorded, RecordingSink, RecordingTransport, Relayed};

/// A device fingerprint the wire accepts.
pub const FINGERPRINT: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
/// A second device, for the cases that need one.
pub const OTHER_FINGERPRINT: &str =
    "bb00000000000000000000000000000000000000000000000000000000000000";
/// A worker fingerprint the wire accepts.
pub const WORKER_FP: &str = "cc00000000000000000000000000000000000000000000000000000000000000";

/// A session id, which must be a uuid.
pub const SESSION: &str = "0f9a1b3c-4d5e-4f60-8a7b-9c0d1e2f3a4b";
/// A second session id.
pub const OTHER_SESSION: &str = "1a2b3c4d-5e6f-4071-9b8c-0d1e2f3a4b5c";
/// A view id, which must be a uuid.
pub const VIEW: &str = "2b3c4d5e-6f70-4182-8c9d-1e2f3a4b5c6d";
/// A second view id.
pub const OTHER_VIEW: &str = "3c4d5e6f-7081-4293-9d0e-2f3a4b5c6d7e";

/// One registered browser socket and the sink behind it.
pub struct Browser {
    /// The socket id the harness registered.
    pub socket_id: String,
    /// The tab-bound viewer key the socket owns.
    pub viewer_key: String,
    /// The sink that records what the socket was told.
    pub sink: Arc<RecordingSink>,
}

/// A hub, its worker registration, and the sockets driving it.
pub struct Harness {
    /// The hub under test.
    pub hub: Arc<TerminalViewHub>,
    /// The transport the owner relay writes through.
    pub transport: Arc<RecordingTransport>,
    /// The owner-mode worker, when the harness registered one.
    pub worker: WorkerFp,
    /// The session every socket is admitted to observe.
    pub session: SessionId,
}

impl Harness {
    /// A hub with one owner-mode worker bound to one session.
    pub fn new() -> Self {
        let hub = Arc::new(TerminalViewHub::new());
        let transport = RecordingTransport::admitting();
        hub.set_owner_transport(transport.clone());
        let worker = WorkerFp::try_from(WORKER_FP).unwrap();
        hub.register_owner(&worker);
        let session = SessionId::try_from(SESSION).unwrap();
        hub.bind_sessions(&worker, std::slice::from_ref(&session));
        Self {
            hub,
            transport,
            worker,
            session,
        }
    }

    /// A hub with no owner-mode worker, so its registry is the only minimizer.
    pub fn unowned() -> Self {
        let hub = Arc::new(TerminalViewHub::new());
        let transport = RecordingTransport::admitting();
        hub.set_owner_transport(transport.clone());
        Self {
            hub,
            transport,
            worker: WorkerFp::try_from(WORKER_FP).unwrap(),
            session: SessionId::try_from(SESSION).unwrap(),
        }
    }

    /// Register a browser socket that may observe the harness's session.
    pub fn browser(&self, socket_id: &str, fingerprint: &str, sessions: &[&str]) -> Browser {
        self.browser_with_tab(socket_id, fingerprint, "tab-1", sessions)
    }

    /// Register a browser socket with an explicit tab id, which is what makes
    /// two tabs on one device two viewer keys.
    pub fn browser_with_tab(
        &self,
        socket_id: &str,
        fingerprint: &str,
        tab: &str,
        sessions: &[&str],
    ) -> Browser {
        let viewer_key = format!("{fingerprint}:{tab}");
        let sink = RecordingSink::new();
        self.hub.register_socket(
            &SocketRegistration {
                socket_id: socket_id.to_owned(),
                viewer_key: Some(viewer_key.clone()),
                caller_fingerprint: fingerprint.to_owned(),
                session_ids: sessions.iter().map(|s| (*s).to_owned()).collect(),
                sink: sink.clone(),
            },
            0,
        );
        Browser {
            socket_id: socket_id.to_owned(),
            viewer_key,
            sink,
        }
    }

    /// Declare a view from one socket.
    pub fn view(
        &self,
        browser: &Browser,
        view_id: &str,
        cols: u32,
        rows: u32,
        revision: u64,
        active: bool,
        now_ms: u64,
    ) {
        self.hub.handle_view_command(
            &browser.socket_id,
            &TerminalViewCommand {
                view_id: view_id.to_owned(),
                session_id: self.session.as_str().to_owned(),
                cols,
                rows,
                revision,
                active,
                domain_generation: 1,
                __buffa_unknown_fields: Default::default(),
            },
            now_ms,
        );
    }

    /// Withdraw a view from one socket.
    pub fn release(&self, browser: &Browser, view_id: &str, revision: u64, now_ms: u64) {
        self.view(browser, view_id, 0, 0, revision, false, now_ms);
    }

    /// The size the session's PTY runs at right now.
    pub fn effective(&self, now_ms: u64) -> Option<(u32, u32)> {
        self.hub
            .session_geometry(&self.session, now_ms)
            .map(|geometry| (geometry.cols, geometry.rows))
    }
}

/// The frames a hub owes a socket, as the decisions they carry.
pub fn decisions(effects: &[Recorded]) -> Vec<(TerminalViewStatus, u32, u32)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Recorded::State {
                status,
                effective_cols,
                effective_rows,
                ..
            } => Some((*status, *effective_cols, *effective_rows)),
            _ => None,
        })
        .collect()
}

/// The sessions a hub told a socket it was watching.
pub fn watching(effects: &[Recorded]) -> Vec<(String, bool)> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Recorded::Watching {
                session_id,
                watching,
            } => Some((session_id.clone(), *watching)),
            _ => None,
        })
        .collect()
}

/// An owner view-state frame, as the worker publishes it.
#[allow(clippy::too_many_arguments)]
pub fn owner_state(
    view_id: &str,
    session_id: &str,
    revision: u64,
    active: bool,
    stream_id: &str,
    cols: u32,
    rows: u32,
) -> TerminalViewStateFrame {
    TerminalViewStateFrame {
        view_id: view_id.to_owned(),
        session_id: session_id.to_owned(),
        revision,
        active,
        stream_id: stream_id.to_owned(),
        status: TerminalViewStatus::Accepted.into(),
        effective_cols: cols,
        effective_rows: rows,
        reason: String::new(),
        __buffa_unknown_fields: Default::default(),
    }
}
