//! The lease façade over opt-in terminal incident capture: what a START, STOP
//! and CAPTURE step does, session teardown, and process shutdown. Ports
//! `apps/worker/src/diag/terminal-capture.ts` (`startTerminalRecording`,
//! `stopTerminalRecording`, `stopTerminalCaptureMaintenance`) and the teardown
//! of `terminal-capture-registry.ts`. `runtime::session_stack` builds the one
//! recorder through [`CaptureRecorder::attach_to_emitter`]; `browser_commands::diagnostics`
//! reaches it as [`DiagnosticReports`].
//!
//! A REPEAT START FROM THE SAME RECORDING RENEWS THE LEASE AND KEEPS EVERY
//! RETAINED RECORD; a different recording on a live lease is a CONFLICT, never
//! a replacement: evicting another operator's evidence is not an answer.

use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex, PoisonError};

use roost_protocol::terminal_capture::{
    TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode as Code, TerminalCaptureWorkerAck,
};
use roost_protocol::viewport::TerminalGeometry;
use roost_protocol::wire::brand::SessionId;
use tokio::runtime::Handle;
use tokio::sync::watch;

use super::ack::{failure_ack, recent_worker_capture_for, recording_ack, stopped_ack};
use super::byte_window::ByteWindows;
use super::now_ms;
use super::recorder_state::{RecorderArming, SegmentRequest, WorkerRecorder};
use super::registry::Registry;
use super::storage::CaptureStorage;
use super::tap::{CaptureShared, CaptureTap};
use super::worker_section::WorkerProcessIdentity;
use super::write::{CaptureSources, capture_terminal_incident};
use crate::browser_commands::diagnostics::{CaptureCommand, DiagnosticReports};
use crate::browser_commands::{Boxed, Refusal};
use crate::diag_snapshot::{Snapshot, SnapshotBuild};
use crate::session::emit::CellEmitter;
use crate::session::lifecycle::{SessionManager, SessionTable};

/// Everything the one recorder is built from.
#[derive(Debug)]
pub struct CaptureRecorderDeps {
    pub table: Arc<SessionTable>,
    pub manager: Arc<SessionManager>,
    /// The worker's log directory: bundles are written directly into it.
    pub log_dir: PathBuf,
    /// This worker process's identity, as every bundle records it.
    pub process: WorkerProcessIdentity,
    pub runtime: Handle,
}

impl CaptureRecorderDeps {
    /// This worker process's deps: the build identity is read once here, the
    /// process id is boot's minted epoch. MUST run inside the tokio runtime.
    pub fn for_process(
        table: &Arc<SessionTable>,
        manager: &Arc<SessionManager>,
        log_dir: &std::path::Path,
        process_epoch: &str,
        worker_fp: &str,
    ) -> Self {
        let identity = roost_host::build_identity(&roost_host::ProcessEnv::new());
        Self {
            table: Arc::clone(table),
            manager: Arc::clone(manager),
            log_dir: log_dir.to_path_buf(),
            process: WorkerProcessIdentity {
                process_id: process_epoch.to_owned(),
                git_sha: identity.build_sha,
                artifact_version: identity.artifact_version,
                worker_fp: worker_fp.to_owned(),
            },
            runtime: Handle::current(),
        }
    }
}
/// This worker's diagnostic reports, and the recorder behind them.
pub struct CaptureRecorder {
    shared: Arc<CaptureShared>,
    sources: CaptureSources,
}

impl std::fmt::Debug for CaptureRecorder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptureRecorder")
            .field("worker_fp", &self.shared.process.worker_fp)
            .field("armed_recordings", &self.shared.registry().armed_count())
            .field("byte_windows", &self.shared.windows().len())
            .finish_non_exhaustive()
    }
}

impl CaptureRecorder {
    pub fn new(deps: CaptureRecorderDeps) -> Self {
        let storage = Arc::new(CaptureStorage::new(deps.log_dir, deps.runtime.clone()));
        let (scheduled, _) = watch::channel(0);
        let shared = Arc::new(CaptureShared {
            registry: Mutex::new(Registry::default()),
            windows: Mutex::new(ByteWindows::default()),
            armed: AtomicUsize::new(0),
            storage,
            process: deps.process,
            runtime: deps.runtime,
            scheduled,
        });
        let sources = CaptureSources {
            table: deps.table,
            manager: deps.manager,
        };
        Self { shared, sources }
    }

    /// The ONE recorder over a session stack: the emitter's data path feeds
    /// its tap, and a closed session drops its window and recorder (v2
    /// `session-lifecycle.ts:243-244`). The hook holds the recorder weakly:
    /// the recorder already holds the manager the hook is registered on.
    pub fn attach_to_emitter(deps: CaptureRecorderDeps, emitter: &Mutex<CellEmitter>) -> Arc<Self> {
        let manager = Arc::clone(&deps.manager);
        let log_dir = deps.log_dir.display().to_string();
        let recorder = Arc::new(Self::new(deps));
        emitter
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .attach_capture(recorder.tap());
        let closing = Arc::downgrade(&recorder);
        manager.on_session_closed(Arc::new(move |session_id: &SessionId| {
            if let Some(recorder) = closing.upgrade() {
                recorder.drop_session(session_id);
            }
        }));
        tracing::info!(
            log_dir,
            "the terminal incident recorder is attached to the session data path"
        );
        recorder
    }
    /// The handle the session data path feeds.
    pub fn tap(&self) -> CaptureTap {
        CaptureTap::attached(Arc::clone(&self.shared))
    }

    /// Session close or channel teardown (v2 `byteCapture.drop` +
    /// `dropTerminalRecorder`): the window, the recorder and the one-shot
    /// ledger go with the session.
    pub fn drop_session(&self, session_id: &SessionId) {
        let session_id = session_id.as_str();
        let armed = {
            let mut registry = self.shared.registry();
            let armed = registry.drop_session(session_id);
            self.shared.note_armed(&registry);
            armed
        };
        self.shared.windows().drop_session(session_id);
        if armed {
            tracing::info!(
                session_id,
                "a session closed; its terminal recording was released"
            );
        }
    }

    /// Process shutdown: the retention sweep is the only thing here that
    /// outlives a session.
    pub fn stop_maintenance(&self) {
        self.shared.storage.stop_capture_retention();
    }

    /// Diagnostic seam (v2 `_settleScheduledCaptures`): wait until every
    /// worker-local capture in flight has written, instead of guessing a delay.
    pub async fn settle_scheduled_captures(&self) {
        let mut scheduled = self.shared.scheduled.subscribe();
        let _ = scheduled.wait_for(|count| *count == 0).await;
    }

    /// Test seam (v2 `terminalRecorderArmed`): whether a live lease is held,
    /// disarming an expired one on observation.
    pub fn _terminal_recorder_armed(&self, session_id: &str) -> bool {
        let mut registry = self.shared.registry();
        let live = registry.disarm_if_expired(session_id);
        self.shared.note_armed(&registry);
        live
    }

    /// Test seam (v2 `_terminalRecorderForTest`): the raw recorder, WITHOUT an
    /// expiry check, so a test can age or inspect it.
    pub fn _with_terminal_recorder<R>(
        &self,
        session_id: &str,
        read: impl FnOnce(Option<&mut WorkerRecorder>) -> R,
    ) -> R {
        let mut registry = self.shared.registry();
        read(
            registry
                .recorders
                .get_mut(session_id)
                .map(|armed| &mut armed.recorder),
        )
    }

    /// v2 `startTerminalRecording`.
    fn start(&self, command: &CaptureCommand) -> TerminalCaptureWorkerAck {
        let now = now_ms();
        let session_id = command.session_id.as_str();
        let stream = self.sources.stream_facts(&command.session_id);
        let entry = self.sources.record(&command.session_id);
        let record = entry.as_ref().map(|entry| {
            entry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        let mut registry = self.shared.registry();
        if let Some(existing) = registry.active(session_id) {
            let recent = recent_worker_capture_for(
                existing.ledger.recent_worker_local.as_ref(),
                &command.capture_id,
            );
            if existing.recorder.recording_id != command.recording_id {
                tracing::warn!(
                    session_id,
                    requested = %command.recording_id,
                    held_by = %existing.recorder.recording_id,
                    "a second recording was refused a live lease"
                );
                return failure_ack(
                    Code::LeaseConflict,
                    recent,
                    Some(existing.recorder.expires_at_ms),
                );
            }
            existing.recorder.expires_at_ms = now + TERMINAL_CAPTURE_LIMITS.lease_ms;
            tracing::info!(
                session_id,
                recording_id = %command.recording_id,
                expires_at_ms = existing.recorder.expires_at_ms,
                "a terminal recording lease was renewed"
            );
            return recording_ack(existing.recorder.expires_at_ms, recent);
        }
        let Some(record) = record.as_deref() else {
            return failure_ack(Code::SessionUnknown, None, None);
        };
        if registry.armed_count() >= TERMINAL_CAPTURE_LIMITS.max_recordings_per_process {
            return failure_ack(Code::ResourceExhausted, None, None);
        }
        let mut recorder = WorkerRecorder::new(RecorderArming {
            session_id: session_id.to_owned(),
            worker_fp: self.shared.process.worker_fp.clone(),
            recording_id: command.recording_id.clone(),
            expires_at_ms: now + TERMINAL_CAPTURE_LIMITS.lease_ms,
            at_ms: now,
            head_seq: record.head_seq,
        });
        // The first segment opens NOW, so raw bytes arriving before the next
        // emission still land in an orderable generation.
        let stream_id = stream.map_or_else(
            || record.cell_emit.stream_id.clone(),
            |stream| stream.stream_id,
        );
        let core = record.terminal_core.as_ref();
        let opened = recorder.open_worker_segment(&SegmentRequest {
            stream_id: &stream_id,
            grid_epoch: &record.cell_emit.grid_epoch(),
            grid_epoch_base: &record.cell_emit.grid_epoch_base,
            geometry: TerminalGeometry {
                cols: u32::from(core.cols()),
                rows: u32::from(core.rows()),
            },
            head_seq: record.head_seq,
            at_ms: now,
        });
        if let Err(error) = opened {
            tracing::error!(
                session_id,
                %error,
                "a terminal recording could not open its first segment"
            );
            return failure_ack(Code::Internal, None, None);
        }
        let (expires_at_ms, armed_offset) = (recorder.expires_at_ms, recorder.armed_offset);
        registry.register(recorder);
        self.shared.note_armed(&registry);
        drop(registry);
        self.shared.storage.ensure_capture_retention();
        tracing::info!(
            session_id,
            recording_id = %command.recording_id,
            expires_at_ms,
            armed_offset,
            "terminal.capture_started"
        );
        recording_ack(expires_at_ms, None)
    }

    /// v2 `stopTerminalRecording`: release the lease and free every retained
    /// record. Saved files are NOT deleted, and a repeat STOP is harmless.
    fn stop(&self, command: &CaptureCommand) -> TerminalCaptureWorkerAck {
        let session_id = command.session_id.as_str();
        let mut registry = self.shared.registry();
        if !registry.disarm_if_expired(session_id) {
            self.shared.note_armed(&registry);
            let one_shot = registry.one_shot.ledger_if_present(session_id);
            let recent = one_shot.and_then(|ledger| {
                recent_worker_capture_for(ledger.recent_worker_local.as_ref(), &command.capture_id)
            });
            return stopped_ack(recent);
        }
        let Some(armed) = registry.recorders.get(session_id) else {
            return stopped_ack(None);
        };
        if armed.recorder.recording_id != command.recording_id {
            return failure_ack(
                Code::PermissionDenied,
                None,
                Some(armed.recorder.expires_at_ms),
            );
        }
        let recent = recent_worker_capture_for(
            armed.ledger.recent_worker_local.as_ref(),
            &command.capture_id,
        );
        registry.forget_recorder(session_id);
        self.shared.note_armed(&registry);
        tracing::info!(
            session_id,
            recording_id = %command.recording_id,
            "terminal.capture_stopped"
        );
        stopped_ack(recent)
    }
}

impl DiagnosticReports for CaptureRecorder {
    /// The state report is the snapshot module's fold, not this recorder's: a
    /// second implementation here would be a second answer to it.
    fn snapshot(&self) -> Result<Snapshot, Refusal> {
        let process = &self.shared.process;
        Ok(Snapshot::of_live_sessions(
            &self.sources.table,
            self.sources.manager.worker_fingerprint(),
            self.sources.manager.cells(),
        )
        .with_build(SnapshotBuild {
            git_sha: Some(process.git_sha.clone()),
            artifact_version: Some(process.artifact_version.clone()),
        }))
    }

    fn start_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck {
        self.start(&command)
    }

    fn stop_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck {
        self.stop(&command)
    }

    /// CAPTURE freezes synchronously inside this call and only then awaits the
    /// compression and the write, so the PTY is never held for either. The
    /// future OWNS what it needs: a capability cannot lend it a borrow.
    fn capture(&self, command: CaptureCommand) -> Boxed<TerminalCaptureWorkerAck> {
        let shared = Arc::clone(&self.shared);
        let sources = self.sources.clone();
        Box::pin(async move { capture_terminal_incident(&shared, &sources, command).await })
    }
}
