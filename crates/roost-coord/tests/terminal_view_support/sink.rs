//! The two recorders every terminal view test needs: a sink that logs each
//! host effect instead of delivering it, and a transport that logs each relay
//! instead of writing to a worker socket.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_coord::terminal_view::{OwnerViewTransport, RelayIdentity};
use roost_proto::{
    FirehoseFrame, TerminalResyncCommand, TerminalViewCommand, TerminalViewStatus,
};
use roost_protocol::wire::{SessionId, WorkerFp};

/// One host effect a socket was asked to perform.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// A view-state frame was queued.
    State {
        /// The session the frame is stamped for.
        session_id: String,
        /// The decision the frame carries.
        status: TerminalViewStatus,
        /// The columns the coordinator reported.
        effective_cols: u32,
        /// The rows the coordinator reported.
        effective_rows: u32,
        /// The stream the coordinator reported, empty for a refusal.
        stream_id: String,
    },
    /// The socket's watch state for a session changed.
    Watching {
        /// The session.
        session_id: String,
        /// Whether the socket still holds a view of it.
        watching: bool,
    },
    /// The socket owed a baseline.
    Seeded(String),
    /// The socket asked to be served forward from a checkpoint.
    Resynced(String),
    /// The coordinator reported a lapsed lease on a socket.
    Expired(String),
    /// The replica was told it cannot serve a session.
    Invalidated(String),
    /// The replica was told which stream to expect.
    Expected {
        /// The stream id.
        stream_id: String,
        /// The columns it carries.
        cols: u32,
        /// The rows it carries.
        rows: u32,
    },
}

/// A sink that records everything instead of delivering it.
#[derive(Debug, Default)]
pub struct RecordingSink {
    effects: Mutex<Vec<Recorded>>,
    seeds_baseline: Mutex<bool>,
}

impl RecordingSink {
    /// A sink whose replica has no valid baseline for any session.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// A sink whose replica can serve a baseline.
    pub fn with_baseline() -> Arc<Self> {
        Arc::new(Self {
            effects: Mutex::new(Vec::new()),
            seeds_baseline: Mutex::new(true),
        })
    }

    /// Everything the sink was asked to do, in order.
    pub fn effects(&self) -> Vec<Recorded> {
        self.effects.lock().unwrap().clone()
    }

    /// The view-state frames the sink was handed.
    pub fn states(&self) -> Vec<Recorded> {
        self.effects()
            .into_iter()
            .filter(|effect| matches!(effect, Recorded::State { .. }))
            .collect()
    }

    fn push(&self, recorded: Recorded) {
        self.effects.lock().unwrap().push(recorded);
    }
}

impl TerminalViewSink for RecordingSink {
    fn enqueue_terminal_state(&self, _socket_id: &str, frame: FirehoseFrame, session_id: &str) {
        let Some(roost_proto::__buffa::oneof::firehose_frame::Frame::TerminalViewState(state)) =
            &frame.frame
        else {
            return;
        };
        self.push(Recorded::State {
            session_id: session_id.to_owned(),
            status: state.status,
            effective_cols: state.effective_cols,
            effective_rows: state.effective_rows,
            stream_id: state.stream_id.clone(),
        });
    }

    fn set_watching(&self, _socket_id: &str, session_id: &SessionId, watching: bool) {
        self.push(Recorded::Watching {
            session_id: session_id.as_str().to_owned(),
            watching,
        });
    }

    fn seed_socket(&self, _socket_id: &str, session_id: &SessionId) -> bool {
        self.push(Recorded::Seeded(session_id.as_str().to_owned()));
        *self.seeds_baseline.lock().unwrap()
    }

    fn resync_socket(
        &self,
        _socket_id: &str,
        session_id: &SessionId,
        _grid_epoch: &str,
        _seq: u64,
    ) {
        self.push(Recorded::Resynced(session_id.as_str().to_owned()));
    }

    fn live_view_expired(&self, _socket_id: &str, _view_id: &str, session_id: &SessionId) {
        self.push(Recorded::Expired(session_id.as_str().to_owned()));
    }

    fn expect_stream(&self, _session_id: &SessionId, stream_id: &str, cols: u32, rows: u32) {
        self.push(Recorded::Expected {
            stream_id: stream_id.to_owned(),
            cols,
            rows,
        });
    }

    fn expected_stream_id(&self, _session_id: &SessionId) -> Option<String> {
        None
    }

    fn invalidate(&self, session_id: &SessionId, _reason: &str) {
        self.push(Recorded::Invalidated(session_id.as_str().to_owned()));
    }
}

/// One relay the transport was asked to make.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Relayed {
    /// A view command went to a worker.
    View {
        /// The worker it went to.
        worker_fp: WorkerFp,
        /// The socket it came from.
        socket_id: String,
        /// The view id it carried.
        view_id: String,
    },
    /// A resync went to a worker.
    Resync {
        /// The worker it went to.
        worker_fp: WorkerFp,
        /// The socket it came from.
        socket_id: String,
    },
    /// A closed socket was announced to a worker.
    Closed {
        /// The worker it was announced to.
        worker_fp: WorkerFp,
        /// The socket that closed.
        socket_id: String,
    },
}

/// A transport that records every relay and admits, or drops, all of them.
#[derive(Debug, Default)]
pub struct RecordingTransport {
    relayed: Mutex<Vec<Relayed>>,
    admit: Mutex<bool>,
}

impl RecordingTransport {
    /// A transport that admits everything.
    pub fn admitting() -> Arc<Self> {
        Arc::new(Self {
            relayed: Mutex::new(Vec::new()),
            admit: Mutex::new(true),
        })
    }

    /// A transport that drops every write, as a fenced worker would.
    pub fn dropping() -> Arc<Self> {
        Arc::new(Self {
            relayed: Mutex::new(Vec::new()),
            admit: Mutex::new(false),
        })
    }

    /// Everything the transport was asked to send, in order.
    pub fn relayed(&self) -> Vec<Relayed> {
        self.relayed.lock().unwrap().clone()
    }
}

impl OwnerViewTransport for RecordingTransport {
    fn relay_view(
        &self,
        worker_fp: &WorkerFp,
        identity: &RelayIdentity,
        command: &TerminalViewCommand,
    ) -> bool {
        self.relayed.lock().unwrap().push(Relayed::View {
            worker_fp: worker_fp.clone(),
            socket_id: identity.socket_id.clone(),
            view_id: command.view_id.clone(),
        });
        *self.admit.lock().unwrap()
    }

    fn relay_resync(
        &self,
        worker_fp: &WorkerFp,
        identity: &RelayIdentity,
        _command: &TerminalResyncCommand,
    ) -> bool {
        self.relayed.lock().unwrap().push(Relayed::Resync {
            worker_fp: worker_fp.clone(),
            socket_id: identity.socket_id.clone(),
        });
        *self.admit.lock().unwrap()
    }

    fn socket_closed(&self, worker_fp: &WorkerFp, socket_id: &str) -> bool {
        self.relayed.lock().unwrap().push(Relayed::Closed {
            worker_fp: worker_fp.clone(),
            socket_id: socket_id.to_owned(),
        });
        *self.admit.lock().unwrap()
    }
}
