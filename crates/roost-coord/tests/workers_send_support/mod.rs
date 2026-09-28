//! A worker link with no socket: a registry holding one ready generation whose
//! `send` records every downstream frame and answers with a chosen sequence,
//! and the relay the terminal senders correlate through.
//!
//! Shared by the `workers_send_*` tests. No database: the senders read only
//! the registry and the pending table.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, PoisonError};

use roost_coord::coord_core::worker_handle::{WorkerHandle, WorkerRegistry};
use roost_coord::terminal_screen::scrollback_relay::ScrollbackRelay;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, InputResult, TerminalInputStatus, TerminalWritePhase,
};
use roost_protocol::wire::{SessionId, WorkerFp};

pub const WORKER_FP: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1";
pub const OTHER_FP: &str = "b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2";
pub const SESSION_ID: &str = "11111111-1111-4111-8111-111111111111";

/// One test's registry, relay and recorded socket.
pub struct TerminalLink {
    pub relay: ScrollbackRelay,
    pub handle: Option<Arc<WorkerHandle>>,
    frames: Arc<Mutex<Vec<CoordWorkerDownstream>>>,
}

impl TerminalLink {
    /// A ready generation whose socket takes every frame.
    pub fn routable() -> Self {
        Self::with_answer(1)
    }

    /// A ready generation whose socket takes nothing: the transport's own
    /// "dropped" answer.
    pub fn dropping() -> Self {
        Self::with_answer(0)
    }

    /// A registry with no generation for the worker at all.
    pub fn offline() -> Self {
        Self {
            relay: ScrollbackRelay::new(Arc::new(WorkerRegistry::new())),
            handle: None,
            frames: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_answer(answer: i64) -> Self {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&frames);
        let send: Arc<dyn Fn(CoordWorkerDownstream) -> i64 + Send + Sync> =
            Arc::new(move |frame: CoordWorkerDownstream| {
                recorded
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(frame);
                answer
            });
        let handle = Arc::new(WorkerHandle::new(
            worker(),
            None,
            "generation-1".to_owned(),
            BTreeSet::new(),
            send,
        ));
        handle.mark_ready();
        let registry = Arc::new(WorkerRegistry::new());
        registry.insert(Arc::clone(&handle));
        Self {
            relay: ScrollbackRelay::new(registry),
            handle: Some(handle),
            frames,
        }
    }

    /// Every frame the socket was handed, in order.
    pub fn frames(&self) -> Vec<CoordWorkerDownstream> {
        self.frames
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many correlation entries are open.
    pub fn pending_count(&self) -> usize {
        self.relay.pending().pending_count()
    }
}

pub fn worker() -> WorkerFp {
    WorkerFp::try_from(WORKER_FP.to_owned()).unwrap()
}

pub fn session() -> SessionId {
    SessionId::try_from(SESSION_ID).unwrap()
}

/// A keeper-completed input result for `request_id`.
pub fn accepted_input(request_id: &str, written_bytes: u32) -> InputResult {
    InputResult {
        request_id: request_id.to_owned(),
        session_id: session(),
        input_seq: 1,
        status: TerminalInputStatus::Accepted,
        written_bytes,
        reason: String::new(),
        phase: TerminalWritePhase::Written,
    }
}

/// The message text of an error, for an assertion.
pub fn message_of(error: &connectrpc::ConnectError) -> &str {
    error.message.as_deref().unwrap_or_default()
}
