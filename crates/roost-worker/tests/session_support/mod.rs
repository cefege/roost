//! The collaborators the session lifecycle is exercised through: one fake each,
//! recording what it was asked, with the real `event_store::Store` behind the
//! sink so a `Reservation` is minted by the only thing that can mint one.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]
// Both of this module's files are compiled into FOUR separate test binaries by
// `#[path]` — `session_adoption`, `session_binding`, `session_lifecycle`,
// `session_resize` — and each binary calls a different subset, so a dead-code
// warning here is a statement about ONE binary rather than about the fixture.
// `fakes.rs` carries the full reading, including which binary calls what and
// why narrowing or deleting an item to quiet one would break a caller in
// another.

mod fakes;

// Only the two the four test binaries actually name are re-exported. The rest
// are imported privately because `Harness` below is their only consumer, and a
// `pub use` nothing reaches is its own lint in a private module.
pub use fakes::{PinnedClock, ScriptedKeeper};

use fakes::{
    CountingCells, FixedResolver, NeverSpawns, RecordingDelivery, RecordingSink, shared_delivery,
};

use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp};
use roost_term::AlacrittyCore;
use roost_term::CellEmitState;
use roost_worker::event_store::DurableEventKind;
use roost_worker::session::binding::{CellDelivery, ChannelDelivery};
use roost_worker::session::lifecycle::{SessionManager, SessionTable};
use roost_worker::session::resume::{AdoptionRequest, KeeperChannels};
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::sinks::SessionEventSink;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::{SHELL_SPEC_VERSION, ShellSpec};

pub const SESSION: &str = "00000000-0000-4000-8000-00000000beef";
pub const OTHER: &str = "00000000-0000-4000-8000-00000000cafe";
// NOT a uuid, unlike `SESSION` and `OTHER` above it. `WorkerFp` is the SHA-256
// hex of a worker's ed25519 pubkey — 64 lowercase hex characters and nothing
// else, with no `sha256:` prefix (`roost_protocol::wire::WorkerFp::check`).
// Sitting among three uuids it was read as one, and every consumer of this
// fixture failed in `worker_fp()` before reaching the behaviour under test.
pub const FINGERPRINT: &str = "00000000000000000000000000000000000000000000000000000000000f00d";
pub const NOW: i64 = 1_700_000_000_000;

pub fn session_id(value: &str) -> SessionId {
    SessionId::try_from(value).expect("the fixture id is a uuid")
}

pub fn channel(value: i64) -> ChannelId {
    ChannelId::try_from(value).expect("a small channel id is in range")
}

fn worker_fp() -> WorkerFp {
    WorkerFp::try_from(FINGERPRINT).expect("the fixture fingerprint is 64 lowercase hex")
}

fn trace() -> TraceId {
    TraceId::try_from("0000beef0000beef").expect("hex of the right length")
}

fn shell_spec(cwd: &str) -> ShellSpec {
    ShellSpec {
        version: SHELL_SPEC_VERSION,
        platform: roost_host::HostPlatform::Linux,
        executable: "/bin/bash".to_string(),
        argv: vec!["-l".to_string()],
        cwd: cwd.to_string(),
        env: Vec::new(),
    }
}

/// The manager, with every collaborator a test can reach.
pub struct Harness {
    pub manager: Arc<SessionManager>,
    pub table: Arc<SessionTable>,
    pub sink: Arc<RecordingSink>,
    pub keeper: Arc<ScriptedKeeper>,
    pub delivery: Arc<RecordingDelivery>,
    pub cells: Arc<Mutex<CountingCells>>,
}

impl Harness {
    pub fn with_keeper(keeper: Arc<ScriptedKeeper>) -> Self {
        let table = Arc::new(SessionTable::default());
        let sink = Arc::new(RecordingSink::default());
        let delivery = Arc::new(RecordingDelivery::default());
        let cells = Arc::new(Mutex::new(CountingCells::default()));
        let manager = Arc::new(SessionManager::new(
            worker_fp(),
            Arc::clone(&table),
            Arc::clone(&sink) as Arc<dyn SessionEventSink>,
            Arc::clone(&keeper) as Arc<dyn KeeperChannels>,
            Arc::clone(&cells) as Arc<Mutex<dyn CellDelivery>>,
            shared_delivery(&delivery),
            Arc::new(PinnedClock),
            Arc::new(NeverSpawns),
            Arc::new(FixedResolver {
                spec: shell_spec("/home/user/project"),
            }),
        ));
        Self {
            manager,
            table,
            sink,
            keeper,
            delivery,
            cells,
        }
    }

    pub fn new() -> Self {
        Self::with_keeper(Arc::new(ScriptedKeeper::default()))
    }

    /// The delivery a hand-built binding is fed through, wired to the same
    /// recorder `self.delivery` exposes, so the assertions keep reading the
    /// one they were written against.
    pub fn shared_delivery(&self) -> Arc<Mutex<dyn ChannelDelivery>> {
        shared_delivery(&self.delivery)
    }

    /// Put a record in the table the way a spawn would have.
    pub fn install(&self, session: &str, channel_id: u16, cwd: &str, spawn_cwd: &str) {
        let record = SessionRecord::new(
            SessionIdentity {
                session_id: session_id(session),
                channel_id: channel(channel_id as i64),
                socket_path: "mux:1".to_string(),
                cwd: cwd.to_string(),
                shell_spec: shell_spec(spawn_cwd),
                session_trace_id: trace(),
                spawned_at_ms: NOW,
            },
            self.sink
                .reserve(DurableEventKind::Closed)
                .expect("a fresh store has room"),
            Box::new(AlacrittyCore::new(80, 24)),
            CellEmitState::new("epoch", "stream"),
            ScrollbackRing::default(),
        );
        self.table.insert(record).expect("a fresh id is free");
    }

    pub fn adoption(&self, session: &str, channel_id: u16, folder: &str) -> AdoptionRequest {
        AdoptionRequest {
            session_id: session_id(session),
            channel_id: channel(channel_id as i64),
            folder: folder.to_string(),
            shell_spec: shell_spec("/home/user/project"),
            session_trace_id: trace(),
            stream_id: "stream-1".to_string(),
            close_reservation: self
                .sink
                .reserve(DurableEventKind::Closed)
                .expect("a fresh store has room"),
            socket_path: "mux:1".to_string(),
            now_ms: NOW,
            mono_ms: 5_000,
        }
    }
}
