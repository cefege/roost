//! One fake per capability the dispatch routes to, each recording what it was
//! asked for so a test can assert on the narrowing rather than on a copy of
//! it.

use std::sync::Mutex;

use roost_protocol::terminal_capture::{
    TerminalCaptureErrorCode, TerminalCaptureFileRef, TerminalCaptureStatus,
    TerminalCaptureWorkerAck,
};
use roost_protocol::wire::brand::SessionId;
use roost_worker::browser_commands::diagnostics::{CaptureCommand, DiagnosticReports};
use roost_worker::browser_commands::presence::PresenceReports;
use roost_worker::browser_commands::scrollback_page::{GridDescription, RetainedGrid};
use roost_worker::browser_commands::search::{BatchSearch, ScrollbackSearch, SingleSearch};
use roost_worker::browser_commands::session_lifecycle::{SessionLifecycle, SessionOutcome};
use roost_worker::browser_commands::{Boxed, Refusal};
use roost_worker::diag_snapshot::Snapshot;
use roost_worker::scrollback_read::EpochBinding;
use serde_json::{Value, json};

use super::EPOCH;

#[derive(Debug, Default)]
pub struct FakeSessions {
    pub killed: Mutex<Vec<String>>,
    pub spawned: Mutex<Vec<String>>,
    pub respawned: Mutex<Vec<String>>,
    pub attached: Mutex<Vec<String>>,
}

impl SessionLifecycle for FakeSessions {
    fn kill(
        &self,
        session_id: SessionId,
    ) -> roost_worker::browser_commands::Boxed<Result<SessionOutcome, Refusal>> {
        self.killed
            .lock()
            .expect("held")
            .push(session_id.to_string());
        Box::pin(async { Ok(SessionOutcome::Killed) })
    }

    fn spawn_shell(
        &self,
        folder: String,
        _cols: Option<u16>,
        _rows: Option<u16>,
        _requested_session_id: Option<SessionId>,
    ) -> roost_worker::browser_commands::Boxed<Result<SessionOutcome, Refusal>> {
        self.spawned.lock().expect("held").push(folder);
        Box::pin(async { Ok(SessionOutcome::Spawned { channel_id: 7 }) })
    }

    fn respawn_if_missing(
        &self,
        session_id: SessionId,
        _cwd: String,
        _cols: u16,
        _rows: u16,
    ) -> roost_worker::browser_commands::Boxed<Result<SessionOutcome, Refusal>> {
        self.respawned
            .lock()
            .expect("held")
            .push(session_id.to_string());
        Box::pin(async { Ok(SessionOutcome::AlreadyLive) })
    }

    fn attach(
        &self,
        session_id: SessionId,
        _from_offset: Option<u64>,
    ) -> roost_worker::browser_commands::Boxed<Result<SessionOutcome, Refusal>> {
        self.attached
            .lock()
            .expect("held")
            .push(session_id.to_string());
        Box::pin(async { Ok(SessionOutcome::Attached { replay_offset: 42 }) })
    }
}

#[derive(Debug, Default)]
pub struct FakePresence {
    pub cursors: Mutex<Vec<(String, u16, u16)>>,
    pub titles: Mutex<Vec<(String, String)>>,
    pub left: Mutex<Vec<(String, String)>>,
}

impl PresenceReports for FakePresence {
    fn cursor_moved(&self, session_id: SessionId, col: u16, row: u16) {
        self.cursors
            .lock()
            .expect("held")
            .push((session_id.to_string(), col, row));
    }

    fn titled(&self, session_id: SessionId, title: String) {
        self.titles
            .lock()
            .expect("held")
            .push((session_id.to_string(), title));
    }

    fn viewer_left(&self, session_id: SessionId, browser_id: String) {
        self.left
            .lock()
            .expect("held")
            .push((session_id.to_string(), browser_id));
    }
}

#[derive(Debug, Default)]
pub struct FakeSearch {
    pub single: Mutex<Vec<SingleSearch>>,
    pub batch: Mutex<Vec<BatchSearch>>,
}

impl ScrollbackSearch for FakeSearch {
    fn search(
        &self,
        request: SingleSearch,
    ) -> roost_worker::browser_commands::Boxed<Result<Value, Refusal>> {
        self.single.lock().expect("held").push(request);
        Box::pin(async { Ok(json!({ "matches": [], "stop_reason": "complete" })) })
    }

    fn search_batch(
        &self,
        request: BatchSearch,
    ) -> roost_worker::browser_commands::Boxed<Result<Value, Refusal>> {
        self.batch.lock().expect("held").push(request);
        Box::pin(async { Ok(json!({ "sessions": [], "stop_reason": "complete" })) })
    }
}

#[derive(Debug, Default)]
pub struct FakeDiagnostics {
    pub started: Mutex<Vec<CaptureCommand>>,
    pub stopped: Mutex<Vec<CaptureCommand>>,
}

impl DiagnosticReports for FakeDiagnostics {
    fn snapshot(&self) -> Result<Snapshot, Refusal> {
        Ok(Snapshot::begin(
            std::time::Duration::from_millis(5),
            std::time::Instant::now(),
        ))
    }

    fn start_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck {
        self.started.lock().expect("held").push(command);
        TerminalCaptureWorkerAck {
            status: TerminalCaptureStatus::Recording,
            path: None,
            byte_length: None,
            error: None,
            expires_at_ms: Some(1_800_000),
            recent_worker_capture: None,
        }
    }

    fn stop_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck {
        self.stopped.lock().expect("held").push(command);
        TerminalCaptureWorkerAck::failed(TerminalCaptureErrorCode::Internal)
    }

    fn capture(
        &self,
        _command: CaptureCommand,
    ) -> roost_worker::browser_commands::Boxed<TerminalCaptureWorkerAck> {
        Box::pin(async {
            TerminalCaptureWorkerAck {
                status: TerminalCaptureStatus::Captured,
                path: Some("/var/log/roost3/incident.json.gz".to_owned()),
                byte_length: Some(2_048),
                error: None,
                expires_at_ms: None,
                recent_worker_capture: Some(TerminalCaptureFileRef {
                    capture_id: "3f6b2a10-0000-4000-8000-0000000000ff".to_owned(),
                    path: "/var/log/roost3/earlier.json.gz".to_owned(),
                    byte_length: 11,
                    status: TerminalCaptureStatus::Captured,
                }),
            }
        })
    }
}

#[derive(Debug)]
pub struct FakeGrid {
    total: u32,
    retained_floor: u32,
    resize_replay_floor: u32,
}

impl Default for FakeGrid {
    fn default() -> Self {
        Self {
            total: 100,
            retained_floor: 0,
            resize_replay_floor: 0,
        }
    }
}

impl RetainedGrid for FakeGrid {
    fn describe(
        &self,
        _session_id: SessionId,
    ) -> roost_worker::browser_commands::Boxed<Result<GridDescription, Refusal>> {
        let description = GridDescription {
            binding: EpochBinding::new(EPOCH),
            retained_floor: self.retained_floor,
            resize_replay_floor: self.resize_replay_floor,
            total: self.total,
            cols: 80,
        };
        Box::pin(async move { Ok(description) })
    }

    fn row(
        &self,
        _session_id: SessionId,
        absolute_row: u32,
    ) -> roost_worker::browser_commands::Boxed<Option<Value>> {
        Box::pin(async move {
            Some(
                json!({ "index": absolute_row, "spans": [{ "text": format!("row {absolute_row}") }] }),
            )
        })
    }
}
