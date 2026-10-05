//! The real session manager, the real emitter and delivery, over a scripted
//! keeper and a recording coordinator sink: what the terminal stream tests drive.
//! Mirrors v2 `apps/worker/tests/terminal/terminal-stream-state-harness.ts`,
//! whose fake keeper socket answers resizes, history and terminal state.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use roost_keeper::client_resize::{ResizeOutcome, ResizeRejectReason, ResizeUnknownReason};
use roost_keeper::frames::ChannelBinding as KeeperChannel;
use roost_keeper::payloads::TerminalState;
use roost_observability::clock::SystemClock;
use roost_protocol::cell::CellGridFrame;
use roost_protocol::wire::brand::{ChannelId, SessionId, TraceId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;
use roost_term::{AlacrittyCore, CellEmitState, TerminalCore};
use roost_worker::event_store::{DurableEventKind, Reservation, Store};
use roost_worker::runtime::cell_delivery::TableCellDelivery;
use roost_worker::runtime::channel_delivery::TableChannelDelivery;
use roost_worker::session::binding::{CellDelivery, ChannelDelivery};
use roost_worker::session::cell_sink::CellSink;
use roost_worker::session::emit::CellEmitter;
use roost_worker::session::keeper_channels::{
    InputNotWritten, KeeperChannels, KeeperFault, KeeperInputCommand, SurvivorHistory,
};
use roost_worker::session::lifecycle::{SessionManager, SessionTable};
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::sinks::{
    ChannelBinding, EventFuture, SessionEventError, SessionEventSink,
};
use roost_worker::session::spawn::{ShellSpawner, ShellSpecResolver};
use roost_worker::session::terminal_state::{StreamIntent, WorkerStreamResult};
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::{SHELL_SPEC_VERSION, ShellSpec};
use roost_worker::terminal_core_capacity::{TerminalCoreCapacity, TerminalCoreCapacityOptions};

mod recording_sink;
pub use recording_sink::RecordingSink;

pub const SESSION: &str = "11111111-2222-4333-8444-555555555555";
pub const CHANNEL: u16 = 43;
pub const STREAM_A: &str = "00000000-0000-4000-8000-00000000000a";
pub const STREAM_B: &str = "00000000-0000-4000-8000-00000000000b";
pub const STREAM_C: &str = "00000000-0000-4000-8000-00000000000c";
pub const COLS: u16 = 12;
pub const ROWS: u16 = 6;

pub fn held<T: ?Sized>(lock: &Mutex<T>) -> MutexGuard<'_, T> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn channel() -> ChannelId {
    ChannelId::try_from(i64::from(CHANNEL)).unwrap()
}

pub fn session_id() -> SessionId {
    SessionId::try_from(SESSION).unwrap()
}

/// What the scripted keeper answers one resize with.
#[derive(Debug, Clone, Copy)]
pub enum Answer {
    Ack,
    AckAt { cols: u16, rows: u16 },
    Refuse(ResizeRejectReason),
    Lost,
    NotWritten,
}

/// The keeper, scripted: every resize is recorded and answered from `script`
/// (default: acknowledged exactly); history is `history` or a refusal.
#[derive(Default)]
pub struct ScriptedKeeper {
    pub script: Mutex<VecDeque<Answer>>,
    pub resized: Mutex<Vec<(u16, u64, u16, u16)>>,
    pub history: Mutex<Option<SurvivorHistory>>,
    pub applied: Mutex<Option<TerminalState>>,
}

impl KeeperChannels for ScriptedKeeper {
    fn live_channels(&self) -> Result<Vec<KeeperChannel>, KeeperFault> {
        Ok(Vec::new())
    }
    fn channel_history(&self, _channel_id: u16) -> Result<SurvivorHistory, KeeperFault> {
        held(&self.history).clone().ok_or_else(|| KeeperFault {
            operation: "channel_history",
            reason: "ordered history is unavailable".to_owned(),
        })
    }
    fn terminal_state(&self, _channel_id: u16) -> Result<TerminalState, KeeperFault> {
        Ok(held(&self.applied).unwrap_or(TerminalState {
            applied_seq: 0,
            cols: COLS,
            rows: ROWS,
        }))
    }
    fn reattach_with_history(
        &self,
        channel_id: u16,
        _pid: u32,
        _binding: Arc<dyn ChannelBinding>,
    ) -> Result<SurvivorHistory, KeeperFault> {
        self.channel_history(channel_id)
    }
    fn kill_channel(&self, _channel_id: u16) -> Result<(), KeeperFault> {
        Ok(())
    }
    fn resize_channel(
        &self,
        channel_id: u16,
        seq: u64,
        cols: u16,
        rows: u16,
    ) -> Result<ResizeOutcome, KeeperFault> {
        let answer = held(&self.script).pop_front().unwrap_or(Answer::Ack);
        if !matches!(answer, Answer::NotWritten) {
            held(&self.resized).push((channel_id, seq, cols, rows));
        }
        match answer {
            Answer::Ack => Ok(ResizeOutcome::Applied { seq, cols, rows }),
            Answer::AckAt { cols, rows } => Ok(ResizeOutcome::Applied { seq, cols, rows }),
            Answer::Refuse(reason) => Ok(ResizeOutcome::Refused { seq, reason }),
            Answer::Lost => Ok(ResizeOutcome::Unknown {
                seq,
                reason: ResizeUnknownReason::Timeout,
            }),
            Answer::NotWritten => Err(KeeperFault {
                operation: "resize_channel",
                reason: "disconnected".to_owned(),
            }),
        }
    }
    fn begin_input(&self, _channel_id: u16, _bytes: Vec<u8>) -> KeeperInputCommand {
        KeeperInputCommand::not_written(InputNotWritten::Disconnected)
    }
    fn write_legacy_input(&self, _channel_id: u16, _bytes: &[u8]) -> Result<(), KeeperFault> {
        Ok(())
    }
}

#[derive(Default)]
struct NoEvents(Mutex<Store>);

impl SessionEventSink for NoEvents {
    fn reserve(
        &self,
        kind: DurableEventKind,
    ) -> EventFuture<'_, Result<Reservation, SessionEventError>> {
        let reserved = held(&self.0)
            .reserve_default(kind)
            .map_err(SessionEventError::Reserve);
        Box::pin(std::future::ready(reserved))
    }
    fn hold(&self, _reservation: Reservation) -> EventFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }
    fn release(&self, _reservation: Reservation) -> EventFuture<'_, ()> {
        Box::pin(std::future::ready(()))
    }
    fn emit<'a>(
        &'a self,
        _event: &'a SessionEvent,
        _reservation: Option<Reservation>,
    ) -> EventFuture<'a, Result<(), SessionEventError>> {
        Box::pin(std::future::ready(Ok(())))
    }
}

struct NeverSpawns;

impl ShellSpawner for NeverSpawns {
    fn spawn_channel(
        &self,
        _: ChannelId,
        _: &ShellSpec,
        _: u16,
        _: u16,
        _: Arc<dyn ChannelBinding>,
    ) -> Result<u32, String> {
        Err("this harness opens no pty".to_owned())
    }
    fn kill_channel(&self, _channel_id: ChannelId) {}
}

struct FixedResolver;

impl ShellSpecResolver for FixedResolver {
    fn resolve_shell_spec(&self, _cwd: &str, _session_id: &str) -> Result<ShellSpec, String> {
        Ok(spec())
    }
}

fn spec() -> ShellSpec {
    ShellSpec {
        version: SHELL_SPEC_VERSION,
        platform: roost_host::HostPlatform::Linux,
        executable: "/bin/sh".to_owned(),
        argv: Vec::new(),
        cwd: "/".to_owned(),
        env: Vec::new(),
    }
}

pub struct Harness {
    pub manager: Arc<SessionManager>,
    pub table: Arc<SessionTable>,
    pub keeper: Arc<ScriptedKeeper>,
    pub emitter: Arc<Mutex<CellEmitter>>,
    pub delivery: Arc<Mutex<dyn ChannelDelivery>>,
    pub sink: Arc<RecordingSink>,
    ordinal: Mutex<u32>,
}

impl Harness {
    pub fn new(core: AlacrittyCore) -> Self {
        Self::with_keeper(core, Arc::new(ScriptedKeeper::default()))
    }

    pub fn with_keeper(core: AlacrittyCore, keeper: Arc<dyn KeeperChannels>) -> Self {
        let scripted = Arc::new(ScriptedKeeper::default());
        Self::build(core, keeper, scripted)
    }

    fn build(
        core: AlacrittyCore,
        keeper: Arc<dyn KeeperChannels>,
        scripted: Arc<ScriptedKeeper>,
    ) -> Self {
        let table = Arc::new(SessionTable::default());
        let cells = TableCellDelivery::new(CellEmitter::new(), Arc::clone(&table));
        let emitter = cells.emitter();
        let delivery: Arc<Mutex<dyn ChannelDelivery>> =
            Arc::new(Mutex::new(TableChannelDelivery::new(
                Arc::clone(&emitter),
                Arc::new(roost_worker::session::terminal_changed::TerminalChangedHooks::default()),
            )));
        let capacity = TerminalCoreCapacity::new(TerminalCoreCapacityOptions {
            effective_memory_ceiling_bytes: 64 << 30,
            boot_rss_bytes: 0,
            terminal_core_cap: Some(8),
        });
        let events = Arc::new(NoEvents::default());
        let manager = SessionManager::new(
            WorkerFp::try_from("0".repeat(64).as_str()).unwrap(),
            Arc::clone(&table),
            Arc::clone(&events) as Arc<dyn SessionEventSink>,
            keeper,
            Arc::new(Mutex::new(cells)) as Arc<Mutex<dyn CellDelivery>>,
            Arc::clone(&delivery),
            Arc::new(SystemClock),
            Arc::new(NeverSpawns),
            Arc::new(FixedResolver),
            capacity,
        );
        let sink = Arc::new(RecordingSink::default());
        held(&emitter).register_sink(Arc::clone(&sink) as Arc<dyn CellSink>);
        let close = held(&events.0)
            .reserve_default(DurableEventKind::Closed)
            .unwrap();
        let record = SessionRecord::new(
            SessionIdentity {
                session_id: session_id(),
                channel_id: channel(),
                socket_path: "/dev/null".to_owned(),
                cwd: "/".to_owned(),
                shell_spec: spec(),
                session_trace_id: TraceId::try_from("0000beef0000beef").unwrap(),
                spawned_at_ms: 1,
            },
            close,
            Box::new(core),
            CellEmitState::new("stream-state-grid", "00000000-0000-4000-8000-000000000001"),
            ScrollbackRing::default(),
        );
        table.insert(record).unwrap();
        Self {
            manager,
            table,
            keeper: scripted,
            emitter,
            delivery,
            sink,
            ordinal: Mutex::new(0),
        }
    }

    /// A harness whose keeper is the scripted one, reachable for scripting.
    pub fn scripted(core: AlacrittyCore) -> Self {
        let keeper = Arc::new(ScriptedKeeper::default());
        Self::build(core, Arc::clone(&keeper) as Arc<dyn KeeperChannels>, keeper)
    }

    pub fn intent(&self, stream_id: &str, enabled: bool, cols: u32, rows: u32) -> StreamIntent {
        let mut ordinal = held(&self.ordinal);
        *ordinal += 1;
        StreamIntent {
            request_id: format!("terminal-stream-test-{ordinal}"),
            session_id: session_id(),
            stream_id: stream_id.to_owned(),
            enabled,
            cols,
            rows,
            budget: None,
        }
    }

    pub async fn enable(&self, stream_id: &str, cols: u16, rows: u16) -> WorkerStreamResult {
        let intent = self.intent(stream_id, true, u32::from(cols), u32::from(rows));
        self.manager.apply_terminal_stream_state(intent).await
    }

    pub fn with_record<R>(&self, read: impl FnOnce(&mut SessionRecord) -> R) -> R {
        let entry = self
            .table
            .record_of_channel(CHANNEL)
            .expect("the session is live");
        let mut record = held(&entry);
        read(&mut record)
    }

    /// A chunk delivered the way the keeper's dispatch thread delivers it.
    pub fn deliver(&self, bytes: &[u8]) {
        let entry = self
            .table
            .record_of_channel(CHANNEL)
            .expect("the session is live");
        let mut record = held(&entry);
        held(&self.delivery).ingest_output(&mut record, bytes, 1_700_000_000_000);
    }

    pub fn core_valid(&self) -> bool {
        self.manager
            .terminal_stream_facts(channel())
            .is_some_and(|facts| facts.core_valid)
    }

    pub fn fulls(&self) -> Vec<CellGridFrame> {
        held(&self.sink.frames)
            .iter()
            .filter(|frame| frame.full)
            .cloned()
            .collect()
    }

    pub fn row_text(frame: &CellGridFrame, row: u32) -> String {
        frame
            .viewport_rows
            .iter()
            .find(|candidate| candidate.index == row)
            .map(|found| found.spans.iter().map(|span| span.text.as_str()).collect())
            .unwrap_or_default()
    }
}

pub fn core_with(cols: u16, rows: u16, text: &[u8]) -> AlacrittyCore {
    let mut core = AlacrittyCore::new(cols, rows);
    core.write(text);
    core
}
