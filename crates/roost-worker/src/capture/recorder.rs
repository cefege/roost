//! The production [`DiagnosticReports`]: this worker's answer about itself, and
//! the opt-in terminal-incident recorder behind `diag-terminal-capture`.
//! `runtime::deps` installs one over the worker's `SessionTable`; nothing else
//! in the worker holds a recorder.
//!
//! It is v2's `apps/worker/src/diag/terminal-capture.ts` and
//! `terminal-capture-registry.ts`. The two rules from them a caller can see are
//! the lease rules, and both are about NOT TAKING SOMEBODY ELSE'S EVIDENCE:
//!
//! A REPEAT START FROM THE SAME RECORDING RENEWS THE LEASE AND KEEPS EVERY
//! RETAINED RECORD. The browser re-sends START on a timer while the debugging
//! pane is visible, so a repeat is the normal case and must not start over.
//!
//! A DIFFERENT RECORDING ON A LIVE LEASE IS A CONFLICT, not a replacement.
//! Evicting another operator's evidence to make room for a second request is
//! never the right answer, and `lease_conflict` says so instead of doing it.
//!
//! LEASE EXPIRY IS DECIDED ON SERVER TIME AND DISARMS ON OBSERVATION. An
//! expired lease is never silently renewed: a browser that went away for half
//! an hour must not find its recording still armed when it comes back.
//!
//! THE ANSWER NEVER CARRIES TERMINAL TEXT. Every failure is one of the
//! protocol's own codes. An `errno` or a parser message from a grid walk quotes
//! the screen it failed on, and this answer crosses a trust boundary into an
//! operator-visible download.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use roost_protocol::terminal_capture::{
    TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode, TerminalCaptureFileRef,
    TerminalCaptureStatus, TerminalCaptureWorkerAck,
};
use serde_json::{Map, Value, json};

use crate::browser_commands::diagnostics::{CaptureAction, CaptureCommand, DiagnosticReports};
use crate::browser_commands::{Boxed, Refusal};
use crate::diag_snapshot::{ChannelDiag, RingBounds, Snapshot};
use crate::session::lifecycle::SessionTable;
use crate::session::types::SessionRecord;

use super::bundle::{Stored, write_bundle};
use super::byte_window::ByteWindow;
use super::leases::{
    Armed, Held, Registry, admit_evidence, failed, now_ms, recording_ack, stopped_ack,
};
/// This worker's diagnostic reports, and the recorder behind them.
pub struct CaptureRecorder {
    registry: Arc<Mutex<Registry>>,
    table: Arc<SessionTable>,
    log_dir: PathBuf,
    worker_fp: String,
}

impl std::fmt::Debug for CaptureRecorder {
    /// The report itself is the diagnostic surface; this is the recorder's own
    /// shape, and the two numbers an operator would ask for first.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let armed = self
            .held()
            .map(|registry| registry.armed.len())
            .unwrap_or_default();
        formatter
            .debug_struct("CaptureRecorder")
            .field("worker_fp", &self.worker_fp)
            .field("armed_recordings", &armed)
            .field(
                "retained_windows",
                &self.held().map(|r| r.windows.len()).unwrap_or(0),
            )
            .finish_non_exhaustive()
    }
}

impl CaptureRecorder {
    /// A recorder over the worker's live sessions, writing under `log_dir`.
    pub fn new(table: Arc<SessionTable>, log_dir: PathBuf, worker_fp: String) -> Self {
        Self {
            registry: Arc::new(Mutex::new(Registry::default())),
            table,
            log_dir,
            worker_fp,
        }
    }

    /// The worker's fingerprint, as a bundle records it.
    pub fn worker_fp(&self) -> &str {
        &self.worker_fp
    }

    /// Retain a chunk of PTY output for the incident stream.
    ///
    /// ALWAYS ON, and the reason is in [`super::byte_window`]: an anomaly fires
    /// when nothing was armed. The armed path costs the same as the unarmed one
    /// here, and the cost is O(chunk) against a fixed-capacity deque.
    pub fn retain_output(&self, session_id: &str, chunk: &[u8]) {
        if chunk.is_empty() {
            return;
        }
        if let Ok(mut registry) = self.held() {
            registry
                .windows
                .entry(session_id.to_owned())
                .or_insert_with(ByteWindow::new)
                .push(chunk);
        }
    }

    /// A session closed: its window and its lease go with it.
    ///
    /// A capture that can still freeze a closed session freezes evidence no
    /// caller can reach, under a lease that answers `Recording` for a session
    /// that is gone.
    pub fn forget_session(&self, session_id: &str) {
        if let Ok(mut registry) = self.held() {
            registry.windows.remove(session_id);
            if registry.armed.remove(session_id).is_some() {
                tracing::info!(
                    session_id,
                    "a session closed; its recording lease was released"
                );
            }
        }
    }

    fn held(&self) -> Held<'_> {
        self.registry.lock()
    }

    /// The armed lease for a session, dropping it if its time has passed.
    ///
    /// `None` covers two different callers — a session that was never armed and
    /// The payload, and the last bundle this worker froze before it.
    fn freeze(
        &self,
        command: &CaptureCommand,
    ) -> Result<(Vec<u8>, Option<TerminalCaptureFileRef>), TerminalCaptureWorkerAck> {
        if command.action != CaptureAction::Capture {
            return Err(TerminalCaptureWorkerAck::failed(
                TerminalCaptureErrorCode::InvalidArgument,
            ));
        }
        let (armed, previous, window) = {
            let mut registry = self
                .held()
                .map_err(|_| failed(TerminalCaptureErrorCode::Internal))?;
            let armed = registry
                .live_lease(command.session_id.as_str())
                .ok_or_else(|| failed(TerminalCaptureErrorCode::LeaseAbsent))?;
            let previous = registry.recent.clone();
            let window = registry
                .windows
                .get(command.session_id.as_str())
                .and_then(ByteWindow::tail);
            (armed, previous, window)
        };
        // A lease with no retained output is `CaptureExpired`, not an empty
        // bundle: a bundle of nothing is a file an operator downloads and finds
        // blank, which is worse than being told the evidence aged out.
        let window = window.ok_or_else(|| failed(TerminalCaptureErrorCode::CaptureExpired))?;
        let identity = self
            .table
            .with_record(&command.session_id, record_identity)
            .ok_or_else(|| failed(TerminalCaptureErrorCode::SessionUnknown))?;

        let mut document = Map::with_capacity(7);
        document.insert(
            "capture_id".to_owned(),
            Value::from(command.capture_id.clone()),
        );
        document.insert(
            "session_id".to_owned(),
            Value::from(command.session_id.as_str()),
        );
        document.insert(
            "recording_id".to_owned(),
            Value::from(command.recording_id.clone()),
        );
        document.insert("reason".to_owned(), Value::from(command.reason.clone()));
        document.insert("frozen_at_ms".to_owned(), Value::from(now_ms()));
        document.insert(
            "worker".to_owned(),
            json!({ "fp": self.worker_fp, "session": identity }),
        );
        document.insert(
            "raw".to_owned(),
            json!({
                "start_offset": window.start_offset,
                "end_offset": window.end_offset,
                "byte_length": window.bytes.len(),
                "base64": base64::engine::general_purpose::STANDARD.encode(&window.bytes),
            }),
        );
        document.insert(
            "evidence".to_owned(),
            json!({
                "browser": armed.browser_evidence,
                "coordinator": armed.coordinator_evidence,
            }),
        );
        let payload = serde_json::to_vec(&Value::Object(document))
            .map_err(|_| failed(TerminalCaptureErrorCode::Internal))?;
        Ok((payload, previous))
    }
}

impl DiagnosticReports for CaptureRecorder {
    /// The state report, folded against one monotonic reading.
    fn snapshot(&self) -> Result<Snapshot, Refusal> {
        let captured_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO);
        let mono_now = std::time::Instant::now();
        let mut channels: HashMap<u16, ChannelDiag> = HashMap::new();
        for (session_id, _) in self.table.live() {
            // `with_record` nests: `None` is "no such session", so the
            // capability's own `Option` is the inner layer.
            if let Some(Some(channel)) = self.table.with_record(&session_id, channel_diag) {
                channels.insert(channel.channel_id, channel);
            }
        }
        Ok(Snapshot::with_channels(captured_at, mono_now, channels))
    }

    /// Arm or renew a recording.
    fn start_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck {
        let mut registry = match self.held() {
            Ok(registry) => registry,
            Err(_) => return failed(TerminalCaptureErrorCode::Internal),
        };
        if let Some(live) = registry.live_lease(command.session_id.as_str()) {
            if live.recording_id != command.recording_id {
                tracing::warn!(
                    session_id = %command.session_id,
                    requested = %command.recording_id,
                    held_by = %live.recording_id,
                    "a second recording was refused a live lease"
                );
                return TerminalCaptureWorkerAck::failed(TerminalCaptureErrorCode::LeaseConflict)
                    .with_recent_worker_capture(registry.recent.clone(), &command.capture_id);
            }
            let expires_at_ms = now_ms().saturating_add(TERMINAL_CAPTURE_LIMITS.lease_ms);
            if let Some(armed) = registry.armed.get_mut(command.session_id.as_str()) {
                armed.expires_at_ms = expires_at_ms;
            }
            tracing::info!(
                session_id = %command.session_id,
                recording_id = %command.recording_id,
                %expires_at_ms,
                "a terminal recording lease was renewed"
            );
            return recording_ack(Some(expires_at_ms), &command.capture_id, &registry);
        }
        if !self
            .table
            .live()
            .iter()
            .any(|(held, _)| *held == command.session_id)
        {
            return failed(TerminalCaptureErrorCode::SessionUnknown);
        }
        if registry.armed.len() >= TERMINAL_CAPTURE_LIMITS.max_recordings_per_process {
            return failed(TerminalCaptureErrorCode::ResourceExhausted);
        }
        let (browser, coordinator) = match admit_evidence(&command) {
            Ok(evidence) => evidence,
            Err(error) => return failed(error),
        };
        let expires_at_ms = now_ms().saturating_add(TERMINAL_CAPTURE_LIMITS.lease_ms);
        registry.armed.insert(
            command.session_id.as_str().to_owned(),
            Armed {
                recording_id: command.recording_id.clone(),
                expires_at_ms,
                browser_evidence: browser,
                coordinator_evidence: coordinator,
            },
        );
        tracing::info!(
            session_id = %command.session_id,
            recording_id = %command.recording_id,
            %expires_at_ms,
            "a terminal recording was armed"
        );
        recording_ack(Some(expires_at_ms), &command.capture_id, &registry)
    }

    /// Release a recording.
    fn stop_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck {
        let mut registry = match self.held() {
            Ok(registry) => registry,
            Err(_) => return failed(TerminalCaptureErrorCode::Internal),
        };
        let Some(live) = registry.live_lease(command.session_id.as_str()) else {
            // A repeat STOP by the owner is harmless: the lease is already gone
            // and there is nothing left to free, so it is `Stopped` rather than a
            // failure the caller has to tell apart from a real one.
            return stopped_ack(&command.capture_id, &registry);
        };
        if live.recording_id != command.recording_id {
            // Somebody else's evidence is not this caller's to release.
            return failed(TerminalCaptureErrorCode::PermissionDenied);
        }
        registry.armed.remove(command.session_id.as_str());
        tracing::info!(
            session_id = %command.session_id,
            recording_id = %command.recording_id,
            "a terminal recording was released"
        );
        stopped_ack(&command.capture_id, &registry)
    }

    /// Freeze the evidence this worker holds and write one bundle.
    fn capture(&self, command: CaptureCommand) -> Boxed<TerminalCaptureWorkerAck> {
        // The future OWNS what it needs rather than borrowing this: `&self`
        // would tie the returned `Send` future to the borrow of the recorder,
        // and a capability trait cannot ask its caller for a longer lifetime
        // than the dispatch has. The registry is behind an `Arc` for this, and
        // the freeze happens BEFORE the future so nothing it makes is borrowed.
        let log_dir = self.log_dir.clone();
        let registry = Arc::clone(&self.registry);
        let frozen = self.freeze(&command);
        Box::pin(async move {
            let (payload, previous) = match frozen {
                Ok(frozen) => frozen,
                Err(refusal) => return refusal,
            };
            let capture_id = command.capture_id.clone();
            let ack = match write_bundle(&log_dir, &capture_id, &payload).await {
                Stored::Written { path, byte_length } => {
                    let frozen_here = TerminalCaptureFileRef {
                        capture_id: capture_id.clone(),
                        path: path.display().to_string(),
                        byte_length,
                        status: TerminalCaptureStatus::Captured,
                    };
                    // The field names the LAST incident this worker froze, and
                    // the capture being answered is excluded: echoing it there
                    // would claim the worker independently found an incident the
                    // operator requested, and send them looking for a second one
                    // that does not exist.
                    let recent = previous
                        .filter(|last| last.capture_id != capture_id)
                        .unwrap_or_else(|| frozen_here.clone());
                    TerminalCaptureWorkerAck {
                        status: TerminalCaptureStatus::Captured,
                        path: Some(frozen_here.path.clone()),
                        byte_length: Some(byte_length),
                        error: None,
                        expires_at_ms: None,
                        recent_worker_capture: Some(recent),
                    }
                }
                Stored::Refused(error) => failed(error),
            };
            if let Ok(mut held) = registry.lock() {
                held.recent = ack.recent_worker_capture.clone();
            }
            ack
        })
    }
}

/// One channel's diagnostic state, taken once.
fn channel_diag(record: &SessionRecord) -> Option<ChannelDiag> {
    let ring = record.scrollback.len() as u64;
    let cap = record.scrollback.capacity() as u64;
    Some(ChannelDiag {
        // The report is keyed by the u16 the wire names, and the brand is a
        // `u32` newtype: a channel id past `u16::MAX` is a keeper that has
        // opened more channels than the wire can name, so it saturates rather
        // than wrapping onto a real channel's number.
        channel_id: u16::try_from(record.channel_id().as_u32()).unwrap_or(u16::MAX),
        grid_epoch: record.cell_emit.grid_epoch(),
        generation: record.cell_emit.seq,
        suppression: None,
        ring: Some(RingBounds {
            retained_bytes: ring,
            cap_bytes: cap,
            evicting: record.scrollback.evicting(),
        }),
    })
}

/// What a session is, in a bundle, without its terminal content.
fn record_identity(record: &SessionRecord) -> Value {
    json!({
        "channel_id": record.channel_id(),
        "cwd": record.identity.cwd,
        "spawned_at_ms": record.identity.spawned_at_ms,
        "head_seq": record.head_seq,
        "grid_epoch": record.cell_emit.grid_epoch(),
        "cols": record.terminal_core.cols(),
        "rows": record.terminal_core.rows(),
        "git_branch": record.git_branch,
        "git_remote": record.git_remote,
    })
}
