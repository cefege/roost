//! The collaborators the worker's terminal view owner is exercised through: a
//! scripted session port standing in for `SessionManager` (v2 runs the real one
//! over a fake keeper), a recording local socket, a manual clock, and the real
//! `Uplink` receiver the coordinator-bound frames arrive on. Used by
//! `terminal_view_owner.rs` and `terminal_view_relay.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use roost_observability::clock::EventClock;
use roost_proto::{
    TerminalViewCommand, TerminalViewStateFrame, TerminalViewStatus, WTerminalViewProjection,
};
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::cell::types::{CellGridFrame, MouseTracking};
use roost_protocol::wire::brand::{ChannelId, SessionId};
use roost_protocol::wire::coord_worker::{
    CoordWorkerUpstream, TerminalStreamFailureKind, TerminalWritePhase,
};
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};
use roost_worker::session::terminal_state::{StreamIntent, WorkerStreamResult};
use roost_worker::terminal_view::{
    LocalViewRegistration, LocalViewTransport, TerminalViewOwner, TerminalViewOwnerDeps,
    ViewSessionPort,
};
use roost_worker::uplink::{OwnerFuture, UplinkReceiver, channel};

/// A lock whose poisoning is irrelevant to a test's own assertions.
fn held<T>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub const SESSION: &str = "11111111-1111-4111-8111-111111111111";
pub const CHANNEL: u32 = 7;
pub const START_MS: u64 = 1_000;

/// The worker's own words for a refused stream request, so a test can tell
/// the verdict the worker published from one it invented.
pub const INVALID_REQUEST_REASON: &str = "this worker refused the stream request";

pub fn device() -> String {
    "a".repeat(64)
}

pub fn remote_device() -> String {
    "b".repeat(64)
}

pub fn session_id() -> SessionId {
    SessionId::try_from(SESSION.to_owned()).unwrap()
}

/// A fresh view id in the shape the registry admits.
pub fn view_id(seed: u32) -> String {
    format!("{seed:08x}-0000-4000-8000-000000000000")
}

pub fn view_command(view_id: &str, cols: u32, rows: u32, revision: u64) -> TerminalViewCommand {
    TerminalViewCommand {
        view_id: view_id.to_owned(),
        session_id: SESSION.to_owned(),
        cols,
        rows,
        revision,
        active: true,
        ..TerminalViewCommand::default()
    }
}

/// The registry's clock, in milliseconds.
#[derive(Debug)]
pub struct ManualClock(AtomicU64);

impl EventClock for ManualClock {
    fn now_epoch_ms(&self) -> i64 {
        0
    }

    fn mono_ns(&self) -> u64 {
        self.0.load(Ordering::SeqCst) * 1_000_000
    }
}

/// What one apply is scripted to answer.
#[derive(Debug, Clone, Copy)]
pub enum Outcome {
    Commit,
    /// The resize boundary trapped: the stream is installed but its core is
    /// not provable, as v2's fake keeper `trapResizeSeqs` produces.
    CoreFailed,
    /// The stream transaction refused the request itself. Nothing this worker
    /// asks for can produce it, so it is the verdict a protocol violation
    /// earns: fail-closed, and repairable by nothing but a new participant.
    InvalidRequest,
}
/// The stream the session layer last installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub stream_id: String,
    pub cols: u32,
    pub rows: u32,
    pub core_valid: bool,
}
#[derive(Default)]
struct FakeInner {
    installed: Option<Installed>,
    script: VecDeque<Outcome>,
    sinks: BTreeMap<String, Arc<dyn CellSink>>,
    snapshots: Vec<String>,
    /// Every stream generation the owner asked the session layer for, in
    /// order: the count is what distinguishes one desire from a retry loop.
    applies: Vec<String>,
}

/// The session layer, scripted: an apply installs the stream and ships its
/// first full to every sink synchronously, the earliest a real emitter could.
#[derive(Default)]
pub struct FakeSessions {
    inner: Mutex<FakeInner>,
}

impl std::fmt::Debug for FakeSessions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FakeSessions")
            .finish_non_exhaustive()
    }
}

impl FakeSessions {
    pub fn scripted(outcomes: &[Outcome]) -> Self {
        let fake = Self::default();
        held(&fake.inner).script = outcomes.iter().copied().collect();
        fake
    }

    pub fn installed(&self) -> Installed {
        held(&self.inner)
            .installed
            .clone()
            .expect("no terminal stream was installed")
    }

    pub fn has_sink(&self, sink_id: &str) -> bool {
        held(&self.inner).sinks.contains_key(sink_id)
    }

    /// Every stream the owner drove, in order. A redrive is one more entry, so
    /// "this event redrove nothing" is an assertion about this list and not
    /// about what the scripted answer was.
    pub fn applies(&self) -> Vec<String> {
        held(&self.inner).applies.clone()
    }

    pub fn snapshots(&self) -> Vec<String> {
        held(&self.inner).snapshots.clone()
    }

    fn ship_full(&self, stream_id: &str) {
        let sinks: Vec<Arc<dyn CellSink>> = held(&self.inner).sinks.values().cloned().collect();
        let channel = ChannelId::try_from(i64::from(CHANNEL)).unwrap();
        // The recorded socket reads only the value model, so the wire stays
        // empty: this fixture's full is not a valid wire frame.
        let wire = roost_proto::PbCellGridFrame::default();
        for sink in sinks {
            sink.send_frame(channel, &full_frame(stream_id), &wire);
        }
    }
}

impl ViewSessionPort for FakeSessions {
    fn apply_stream_state(&self, intent: StreamIntent) -> OwnerFuture<WorkerStreamResult> {
        let outcome = held(&self.inner)
            .script
            .pop_front()
            .unwrap_or(Outcome::Commit);
        let core_valid = matches!(outcome, Outcome::Commit);
        let mut inner = held(&self.inner);
        inner.applies.push(intent.stream_id.clone());
        inner.installed = Some(Installed {
            stream_id: intent.stream_id.clone(),
            cols: intent.cols,
            rows: intent.rows,
            core_valid,
        });
        drop(inner);
        let result = match outcome {
            Outcome::Commit => {
                self.ship_full(&intent.stream_id);
                WorkerStreamResult::Committed {
                    stream_id: intent.stream_id,
                    enabled: intent.enabled,
                    cols: intent.cols,
                    rows: intent.rows,
                    channel_resize_seq: 1,
                    resized: true,
                }
            }
            Outcome::CoreFailed => WorkerStreamResult::Ambiguous {
                stream_id: intent.stream_id,
                enabled: intent.enabled,
                cols: intent.cols,
                rows: intent.rows,
                channel_resize_seq: 1,
                failure: TerminalStreamFailureKind::CoreFailed,
                reason: "the resize boundary could not be proven".to_owned(),
                phase: TerminalWritePhase::Written,
            },
            Outcome::InvalidRequest => WorkerStreamResult::Rejected {
                stream_id: intent.stream_id,
                enabled: intent.enabled,
                cols: intent.cols,
                rows: intent.rows,
                channel_resize_seq: 0,
                failure: TerminalStreamFailureKind::InvalidRequest,
                reason: INVALID_REQUEST_REASON.to_owned(),
                phase: TerminalWritePhase::PreWrite,
            },
        };
        Box::pin(async move { result })
    }

    fn request_snapshot(&self, _session_id: &SessionId, stream_id: &str) {
        held(&self.inner).snapshots.push(stream_id.to_owned());
        self.ship_full(stream_id);
    }

    fn current_stream_id(&self, session_id: &SessionId) -> Option<String> {
        let inner = held(&self.inner);
        let installed = inner.installed.as_ref()?;
        (session_id.as_str() == SESSION && installed.core_valid)
            .then(|| installed.stream_id.clone())
    }

    fn channel_of(&self, session_id: &SessionId) -> Option<ChannelId> {
        (session_id.as_str() == SESSION).then(|| ChannelId::try_from(i64::from(CHANNEL)).unwrap())
    }

    fn register_cell_sink(&self, sink: Arc<dyn CellSink>) {
        held(&self.inner).sinks.insert(sink.id().to_owned(), sink);
    }

    fn unregister_cell_sink(&self, sink_id: &str) {
        held(&self.inner).sinks.remove(sink_id);
    }
}

/// The complete full the scripted session layer installs, as a keeper would emit it.
pub fn full_frame(stream_id: &str) -> CellGridFrame {
    CellGridFrame {
        stream_id: stream_id.to_owned(),
        grid_epoch: "epoch-1".to_owned(),
        cols: 80,
        rows: 24,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        kitty_keyboard_flags: 0,
        full: true,
        viewport_rows: Vec::new(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq: 1,
    }
}

mod socket;

pub use socket::Fixture;
