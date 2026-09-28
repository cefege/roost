//! Freezes one coordinator-driven CAPTURE: admission in v2's order, the
//! remote evidence, and the worker section — all synchronous, under the record
//! and registry locks — before `super::finish` writes it. Ports
//! `captureTerminalIncident` and `replayCompletedCapture` of `apps/worker/src/
//! diag/terminal-capture-write.ts`; called by `super::recorder`. Remote
//! sections are used exactly as the peers froze them, or are absent.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::{Value, json};

use roost_protocol::terminal_capture::bundle::TerminalCaptureLayer;
use roost_protocol::terminal_capture::envelope::EvidenceOwner;
use roost_protocol::terminal_capture::{
    TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode as Code, TerminalCaptureWorkerAck,
};
use roost_protocol::wire::brand::{ChannelId, SessionId};

use super::ack::{failure_ack, recent_worker_capture_for};
use super::evidence::{
    RemoteEvidence, history_ranges_from_browser_evidence, parse_remote_evidence,
};
use super::finish::{FinishRequest, LedgerOwner, finish_capture};
use super::now_ms;
use super::recorder_state::CaptureLedger;
use super::registry::Registry;
use super::tap::CaptureShared;
use super::worker_section::{StreamFacts, WorkerSectionRequest, freeze_worker_section};
use crate::browser_commands::diagnostics::CaptureCommand;
use crate::session::lifecycle::{SessionManager, SessionTable};
use crate::session::types::SessionRecord;

/// Where a capture reads the live session from.
#[derive(Debug, Clone)]
pub struct CaptureSources {
    pub table: Arc<SessionTable>,
    pub manager: Arc<SessionManager>,
}

impl CaptureSources {
    /// The coordinator-owned stream a session delivers, read with NO lock of
    /// this module held (it takes the stream table's).
    pub fn stream_facts(&self, session_id: &SessionId) -> Option<StreamFacts> {
        let raw = self.table.channel_of(session_id)?;
        let channel = ChannelId::try_from(i64::from(raw)).ok()?;
        let facts = self.manager.terminal_stream_facts(channel)?;
        Some(StreamFacts {
            stream_id: facts.stream_id,
            cols: facts.cols,
            rows: facts.rows,
        })
    }

    pub fn record(&self, session_id: &SessionId) -> Option<Arc<Mutex<SessionRecord>>> {
        self.table
            .record_of_channel(self.table.channel_of(session_id)?)
    }
}

/// Freeze this session's worker evidence, merge the already-frozen remote
/// evidence, and write ONE bundle.
pub async fn capture_terminal_incident(
    shared: &Arc<CaptureShared>,
    sources: &CaptureSources,
    command: CaptureCommand,
) -> TerminalCaptureWorkerAck {
    let owner = EvidenceOwner {
        capture_id: &command.capture_id,
        recording_id: &command.recording_id,
        session_id: command.session_id.as_str(),
    };
    let browser = parse_remote_evidence(
        &command.browser_evidence_json,
        TerminalCaptureLayer::Browser,
        &owner,
    );
    let coordinator = parse_remote_evidence(
        &command.coordinator_evidence_json,
        TerminalCaptureLayer::Coordinator,
        &owner,
    );
    let stream = sources.stream_facts(&command.session_id);
    let entry = sources.record(&command.session_id);
    // The record lock is taken, and released, inside this block: nothing of it
    // may be held across the write below.
    let admitted = {
        let record = entry
            .as_ref()
            .map(|entry| entry.lock().unwrap_or_else(PoisonError::into_inner));
        admit_capture(
            shared,
            CaptureInputs {
                command: &command,
                record: record.as_deref(),
                stream: stream.as_ref(),
                browser,
                coordinator,
            },
        )
    };
    match admitted {
        Ok(request) => finish_capture(shared, request).await,
        Err(answer) => *answer,
    }
}

struct CaptureInputs<'a> {
    command: &'a CaptureCommand,
    record: Option<&'a SessionRecord>,
    stream: Option<&'a StreamFacts>,
    browser: Result<RemoteEvidence, Code>,
    coordinator: Result<RemoteEvidence, Code>,
}

/// v2's admission order, then the freeze — all inside one registry hold.
fn admit_capture(
    shared: &CaptureShared,
    inputs: CaptureInputs<'_>,
) -> Result<FinishRequest, Box<TerminalCaptureWorkerAck>> {
    let command = inputs.command;
    let (session_id, capture_id) = (command.session_id.as_str(), command.capture_id.as_str());
    let now = now_ms();
    let mut guard = shared.registry();
    let registry: &mut Registry = &mut guard;
    registry.disarm_if_expired(session_id);
    let (recorder, ledger) = match registry.recorders.get_mut(session_id) {
        Some(armed) => (Some(&mut armed.recorder), &mut armed.ledger),
        None => (None, registry.one_shot.ledger(session_id)),
    };
    let expires = recorder.as_ref().map(|recorder| recorder.expires_at_ms);
    if let Some(prior) = ledger.completed.get(capture_id) {
        return Err(Box::new(replay_completed_capture(
            prior.clone(),
            capture_id,
            ledger,
            expires,
        )));
    }
    let recent = recent_worker_capture_for(ledger.recent_worker_local.as_ref(), capture_id);
    let refuse =
        |code: Code, expires: Option<u64>| Box::new(failure_ack(code, recent.clone(), expires));
    if recorder
        .as_ref()
        .is_some_and(|recorder| recorder.capture_in_flight)
    {
        return Err(refuse(Code::CaptureInFlight, expires));
    }
    if ledger.completed.len() >= TERMINAL_CAPTURE_LIMITS.completed_capture_ids {
        return Err(refuse(Code::ResourceExhausted, None));
    }
    let manual = command.reason == "manual";
    let cooldown = TERMINAL_CAPTURE_LIMITS.manual_cooldown_ms;
    if manual
        && ledger
            .last_manual_ms
            .is_some_and(|last| now.saturating_sub(last) < cooldown)
    {
        return Err(refuse(Code::RateLimited, None));
    }
    let Some(record) = inputs.record else {
        return Err(refuse(Code::SessionUnknown, None));
    };
    let browser = inputs.browser.map_err(|code| refuse(code, None))?;
    let coordinator = inputs.coordinator.map_err(|code| refuse(code, None))?;
    let history = history_ranges_from_browser_evidence(browser.section.as_ref());
    let frozen = freeze_worker_section(WorkerSectionRequest {
        session_id,
        process: &shared.process,
        recorder: recorder.as_deref(),
        record: Some(record),
        stream: inputs.stream,
        history_ranges: &history,
        windows: &shared.windows(),
        captured_at_ms: now,
    });
    let owner = match recorder {
        Some(recorder) => {
            recorder.capture_in_flight = true;
            LedgerOwner::Armed {
                recording_id: recorder.recording_id.clone(),
                armed_at_ms: recorder.armed_at_ms,
            }
        }
        None => LedgerOwner::OneShot,
    };
    if manual {
        ledger.last_manual_ms = Some(now);
    }
    let stream = frozen.section.stream.as_ref();
    // `origin: "worker"` means the worker detected this incident itself, so a
    // requested capture never carries it, whether or not the browser answered.
    let worker_trigger = json!({
        "reason": command.reason,
        "origin": if browser.section.is_none() { "coordinator" } else { "browser" },
        "at_ms": now,
        "stream_id": stream.map(|stream| stream.stream_id.clone()),
        "grid_epoch": stream.map(|stream| stream.grid_epoch.clone()),
        "seq": stream.map(|stream| stream.seq.clone()),
        "detail": null,
        "occurrence_count": 0,
    });
    // The browser authors the trigger when it is the origin: only it knows
    // WHICH invariant fired and how many occurrences its latch collapsed. The
    // authoring layer stays worker-owned, because it is a structural fact.
    let trigger = match browser.trigger {
        Some(mut peer) => {
            peer.insert("origin".to_owned(), Value::from("browser"));
            Value::Object(peer)
        }
        None => worker_trigger.clone(),
    };
    Ok(FinishRequest {
        capture_id: capture_id.to_owned(),
        recording_id: command.recording_id.clone(),
        session_id: session_id.to_owned(),
        owner,
        frozen,
        worker_local: false,
        trigger,
        worker_trigger,
        coordinator: coordinator.section,
        browser: browser.section,
        ledger_recent: ledger.recent_worker_local.clone(),
    })
}

/// An RPC retry gets the ORIGINAL result. When retention already removed that
/// file the answer is `capture_expired`: recreating it from later terminal
/// state would silently substitute a different incident.
fn replay_completed_capture(
    prior: TerminalCaptureWorkerAck,
    capture_id: &str,
    ledger: &CaptureLedger,
    expires_at_ms: Option<u64>,
) -> TerminalCaptureWorkerAck {
    if prior
        .path
        .as_deref()
        .is_none_or(|path| Path::new(path).exists())
    {
        return prior;
    }
    failure_ack(
        Code::CaptureExpired,
        recent_worker_capture_for(ledger.recent_worker_local.as_ref(), capture_id),
        expires_at_ms,
    )
}
