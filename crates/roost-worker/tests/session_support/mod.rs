//! The collaborators the session lifecycle is exercised through: one fake each,
//! recording what it was asked, with the real `event_store::Store` behind the
//! sink so a `Reservation` is minted by the only thing that can mint one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::history::HistoryRecord;
use roost_keeper::payloads::TerminalState;
use roost_observability::clock::EventClock;
use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;
use roost_term::AlacrittyCore;
use roost_term::CellEmitState;
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::session::binding::ChannelDelivery;
use roost_worker::session::lifecycle::{CellDelivery, SessionManager, SessionTable};
use roost_worker::session::resume::{KeeperChannels, KeeperFault, SurvivorHistory};
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::sinks::{ChannelBinding, SessionEventError, SessionEventSink};
use roost_worker::session::spawn::{ShellSpawner, ShellSpecResolver};
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::{SHELL_SPEC_VERSION, ShellSpec};

pub const SESSION: &str = "00000000-0000-4000-8000-00000000beef";
pub const OTHER: &str = "00000000-0000-4000-8000-00000000cafe";
pub const FINGERPRINT: &str = "00000000-0000-4000-8000-00000000f00d";
pub const NOW: i64 = 1_700_000_000_000;

pub fn session_id(value: &str) -> SessionId {
    SessionId::try_from(value).expect("the fixture id is a uuid")
}

fn channel(value: i64) -> ChannelId {
    ChannelId::try_from(value).expect("a small channel id is in range")
}

fn worker_fp() -> WorkerFp {
    WorkerFp::try_from(FINGERPRINT).expect("the fixture fingerprint is a uuid")
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

/// A clock that does not move, so a timestamp in an assertion is the one the
/// test wrote.
pub struct PinnedClock;

impl EventClock for PinnedClock {
    fn now_epoch_ms(&self) -> i64 {
        NOW
    }
    fn mono_ns(&self) -> u64 {
        5_000_000_000
    }
}

/// The durable boundary, over a real store, recording what it published.
#[derive(Default)]
pub struct RecordingSink {
    pub store: Mutex<Store>,
    pub emitted: Mutex<Vec<SessionEvent>>,
    pub fail_next: Mutex<bool>,
}

impl SessionEventSink for RecordingSink {
    fn reserve(&self, kind: DurableEventKind) -> Result<Reservation, SessionEventError> {
        self.store
            .lock()
            .expect("held")
            .reserve_default(kind)
            .map_err(SessionEventError::Reserve)
    }
    fn hold(&self, reservation: Reservation) {
        self.store
            .lock()
            .expect("held")
            .hold(reservation)
            .expect("a fresh claim is live");
    }
    fn release(&self, reservation: Reservation) {
        let _ = self.store.lock().expect("held").release(reservation);
    }
    fn emit(
        &self,
        event: &SessionEvent,
        reservation: Option<Reservation>,
    ) -> Result<(), SessionEventError> {
        if *self.fail_next.lock().expect("held") {
            return Err(SessionEventError::Unclassifiable(
                "the store is refusing writes".to_string(),
            ));
        }
        if let Some(reservation) = reservation {
            let bytes = serde_json::to_vec(event)
                .expect("an event serialises")
                .len();
            let kind = event_kind(event);
            self.store
                .lock()
                .expect("held")
                .append(reservation, kind, bytes)
                .map_err(SessionEventError::Append)?;
        }
        self.emitted.lock().expect("held").push(event.clone());
        Ok(())
    }
}

impl RecordingSink {
    pub fn published(&self) -> Vec<SessionEvent> {
        self.emitted.lock().expect("held").clone()
    }
    pub fn closed_events(&self) -> usize {
        self.published()
            .iter()
            .filter(|event| matches!(event, SessionEvent::Closed { .. }))
            .count()
    }
}

fn event_kind(event: &SessionEvent) -> DurableEventKind {
    match event {
        SessionEvent::Opened { .. } => DurableEventKind::Opened,
        SessionEvent::Closed { .. } => DurableEventKind::Closed,
        _ => DurableEventKind::State,
    }
}

/// A keeper that answers from a script and remembers what it was told.
#[derive(Default)]
pub struct ScriptedKeeper {
    pub channels: Mutex<Vec<KeeperChannel>>,
    pub history: Mutex<SurvivorHistory>,
    pub applied: Mutex<TerminalState>,
    pub delivered: Mutex<Option<Arc<dyn ChannelBinding>>>,
    pub killed: Mutex<Vec<u16>>,
    pub resized: Mutex<Vec<(u16, u64, u16, u16)>>,
    pub list_fails: Mutex<bool>,
}

impl KeeperChannels for ScriptedKeeper {
    fn live_channels(&self) -> Result<Vec<KeeperChannel>, KeeperFault> {
        if *self.list_fails.lock().expect("held") {
            return Err(KeeperFault {
                operation: "live_channels",
                reason: "the socket went away".to_string(),
            });
        }
        Ok(self.channels.lock().expect("held").clone())
    }
    fn channel_history(&self, _channel_id: u16) -> Result<SurvivorHistory, KeeperFault> {
        Ok(self.history.lock().expect("held").clone())
    }
    fn terminal_state(&self, _channel_id: u16) -> Result<TerminalState, KeeperFault> {
        Ok(*self.applied.lock().expect("held"))
    }
    fn deliver_into(
        &self,
        _channel_id: u16,
        binding: Arc<dyn ChannelBinding>,
    ) -> Result<(), KeeperFault> {
        *self.delivered.lock().expect("held") = Some(binding);
        Ok(())
    }
    fn kill_channel(&self, channel_id: u16) -> Result<(), KeeperFault> {
        self.killed.lock().expect("held").push(channel_id);
        Ok(())
    }
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<(), KeeperFault> {
        self.resized
            .lock()
            .expect("held")
            .push((channel_id, seq, cols, rows));
        Ok(())
    }
}

impl ScriptedKeeper {
    pub fn with_survivor(channel_id: u16, pid: u32) -> Self {
        Self {
            channels: Mutex::new(vec![KeeperChannel { channel_id, pid }]),
            history: Mutex::new(SurvivorHistory {
                base_cols: 80,
                base_rows: 24,
                ..SurvivorHistory::default()
            }),
            applied: Mutex::new(TerminalState {
                applied_seq: 7,
                cols: 80,
                rows: 24,
            }),
            ..ScriptedKeeper::default()
        }
    }
    pub fn delivered(&self) -> Arc<dyn ChannelBinding> {
        self.delivered
            .lock()
            .expect("held")
            .clone()
            .expect("adoption delivers before anything else")
    }
    pub fn killed(&self) -> Vec<u16> {
        self.killed.lock().expect("held").clone()
    }
}

/// The delivery, answering with what it was handed and nothing else.
#[derive(Default)]
pub struct RecordingDelivery {
    pub parsed: Mutex<Vec<Vec<u8>>>,
    pub exits: Mutex<Vec<Option<i32>>>,
    pub frozen: Mutex<Vec<ChannelId>>,
    pub capture: Mutex<Vec<u8>>,
}

impl ChannelDelivery for RecordingDelivery {
    fn ingest_output(&self, _record: &mut SessionRecord, chunk: &[u8], _now_ms: i64) {
        self.parsed.lock().expect("held").push(chunk.to_vec());
    }
    fn ingest_exit(&self, _record: &mut SessionRecord, exit_code: Option<i32>, _now_ms: i64) {
        self.exits.lock().expect("held").push(exit_code);
    }
    fn ingest_error(&self, _record: &mut SessionRecord, _reason: &str, _now_ms: i64) {}
    fn freeze_capture(&self, channel_id: ChannelId) -> bool {
        self.frozen.lock().expect("held").push(channel_id);
        true
    }
    fn close_capture(
        &self,
        _channel_id: ChannelId,
    ) -> roost_worker::session::binding::CapturedOutput {
        roost_worker::session::binding::CapturedOutput {
            bytes: std::mem::take(&mut *self.capture.lock().expect("held")),
            overflowed: false,
        }
    }
}

/// The emitter's channel registration, counted.
#[derive(Default)]
pub struct CountingCells {
    pub installed: Mutex<Vec<(u16, String)>>,
    pub forgotten: Mutex<Vec<u16>>,
}

impl CellDelivery for CountingCells {
    fn install_stream(&mut self, channel_id: ChannelId, stream_id: &str) {
        self.installed
            .lock()
            .expect("held")
            .push((channel_id.as_u32() as u16, stream_id.to_string()));
    }
    fn forget_channel(&mut self, channel_id: ChannelId) {
        self.forgotten
            .lock()
            .expect("held")
            .push(channel_id.as_u32() as u16);
    }
}

/// A spawner that never runs: these tests are about the paths around a spawn.
pub struct NeverSpawns;

impl ShellSpawner for NeverSpawns {
    fn spawn_channel(
        &self,
        _channel_id: ChannelId,
        _spec: &ShellSpec,
        _cols: u16,
        _rows: u16,
        _binding: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String> {
        Err("this harness does not open a pty".to_string())
    }
    fn kill_channel(&self, _channel_id: ChannelId) {}
}

pub struct FixedResolver {
    pub spec: ShellSpec,
}

impl ShellSpecResolver for FixedResolver {
    fn resolve_shell_spec(&self, _cwd: &str, _session_id: &str) -> Result<ShellSpec, String> {
        Ok(self.spec.clone())
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
            Arc::clone(&delivery) as Arc<Mutex<dyn ChannelDelivery>>,
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
