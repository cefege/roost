//! What the worker is willing to say about itself: the state report the
//! coordinator fans out to diagnostics, and one step of an opt-in terminal
//! incident recording. Owned by the worker.
//!
//! ONE PROPERTY SHAPES BOTH. Neither answer may carry text the terminal
//! produced. The snapshot reports ids, bounds, counts and durations, and a
//! capture failure is a fixed code rather than a message — because a parser or
//! validator message from a grid walk quotes the cells it failed on, and both
//! answers cross a trust boundary into an operator-visible download. The
//! capture path is where that is easiest to get wrong, because it is the one
//! that has a real error to report: the code is the report.
//!
//! EVERY CAPTURE STEP IS ANSWERED, INCLUDING THE ONES THAT THREW. A refusal
//! would leave the coordinator's pending entry to expire against a cause
//! nobody wrote down, and a step that is skipped must say it was skipped.

use std::sync::Arc;

use serde_json::Value;

use roost_protocol::terminal_capture::{
    TerminalCaptureErrorCode, TerminalCaptureStatus, TerminalCaptureWorkerAck,
};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::control::ClientControlFrame;

use super::{Answered, Boxed, Command, Deps, Refusal, Reply};
use crate::diag_snapshot::Snapshot;

/// What the worker is willing to say about itself.
pub trait DiagnosticReports: Send + Sync {
    /// The state report, folded against one monotonic reading.
    fn snapshot(&self) -> Result<Snapshot, Refusal>;

    /// Arm or renew a recording.
    fn start_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck;

    /// Release a recording.
    fn stop_recording(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck;

    /// Freeze the evidence this worker holds and write one bundle.
    fn capture(&self, command: CaptureCommand) -> Boxed<TerminalCaptureWorkerAck>;
}

/// One capture step, as the recorder is handed it.
///
/// The browser's evidence and the coordinator's are SEPARATE FIELDS rather
/// than one merged blob, because they have different provenance and different
/// trust: the coordinator's is authoritative and destination-free, and a
/// capture that merged them into one string would have no way to say which
/// half came from where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureCommand {
    pub action: CaptureAction,
    pub session_id: SessionId,
    pub recording_id: String,
    pub capture_id: String,
    pub reason: String,
    pub browser_evidence_json: String,
    pub coordinator_evidence_json: String,
}

/// Which step of a recording this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureAction {
    Start,
    Capture,
    Stop,
}

impl CaptureAction {
    /// The action as the frame spells it, or the refusal for a spelling this
    /// build does not know.
    ///
    /// The frame's own admission already enumerated the actions, so this is
    /// belt to that braces: a frame that arrived from a build with a fourth
    /// action is refused here rather than falling into whichever arm it
    /// happened to match.
    pub fn parse(value: &str) -> Result<Self, Refusal> {
        match value {
            "start" => Ok(Self::Start),
            "capture" => Ok(Self::Capture),
            "stop" => Ok(Self::Stop),
            other => Err(Refusal::failed(
                "diag-terminal-capture",
                format!("`{other}` is not a capture action"),
            )),
        }
    }

    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Capture => "capture",
            Self::Stop => "stop",
        }
    }
}

/// Run whichever diagnostic command arrived.
pub async fn execute(command: &Command, deps: &Deps) -> Result<Answered, Refusal> {
    match &command.frame {
        ClientControlFrame::DiagSnapshot { .. } => {
            let report = deps.diagnostics.snapshot()?;
            Ok(Answered::Reply(Reply::ok(
                &command.request_id,
                snapshot_json(&report),
            )))
        }
        ClientControlFrame::DiagTerminalCapture {
            session_id,
            recording_id,
            capture_id,
            action,
            reason,
            browser_evidence_json,
            coordinator_evidence_json,
            ..
        } => {
            let step = CaptureCommand {
                action: CaptureAction::parse(action)?,
                session_id: session_id.clone(),
                recording_id: recording_id.clone(),
                capture_id: capture_id.clone(),
                reason: reason.clone(),
                browser_evidence_json: browser_evidence_json.clone(),
                coordinator_evidence_json: coordinator_evidence_json.clone(),
            };
            let ack = match step.action {
                CaptureAction::Start => deps.diagnostics.start_recording(step),
                CaptureAction::Stop => deps.diagnostics.stop_recording(step),
                CaptureAction::Capture => deps.diagnostics.capture(step).await,
            };
            Ok(Answered::Reply(Reply::ok(
                &command.request_id,
                serde_json::to_value(ack).unwrap_or_else(|_| {
                    serde_json::json!({
                        "status": TerminalCaptureStatus::Error,
                        "error": TerminalCaptureErrorCode::Internal,
                    })
                }),
            )))
        }
        other => Err(Refusal::failed(
            "diagnostics",
            format!("{} is not a diagnostic command", other.kind()),
        )),
    }
}

/// The state report, as the value a `diag-snapshot` answers with.
///
/// Ages are computed here, against the ONE monotonic reading the report took,
/// and nothing else is: a report that mixed a wall clock into an age would let
/// an NTP correction make a stall appear or vanish.
fn snapshot_json(report: &Snapshot) -> Value {
    let mut channels: Vec<Value> = report
        .channels
        .values()
        .map(|channel| {
            let suppression = channel.suppression.map(|held| {
                let age = report
                    .mono_now
                    .saturating_duration_since(held.since)
                    .as_millis() as u64;
                serde_json::json!({
                    "gate": format!("{:?}", held.gate).to_lowercase(),
                    "since_ms": age,
                    "frames": held.frames,
                    "over_budget": age
                        >= held.gate.budget().as_millis() as u64,
                })
            });
            serde_json::json!({
                "channel_id": channel.channel_id,
                "grid_epoch": channel.grid_epoch,
                "generation": channel.generation,
                "suppression": suppression,
                "ring": channel.ring.map(|ring| serde_json::json!({
                    "retained_bytes": ring.retained_bytes,
                    "cap_bytes": ring.cap_bytes,
                    "evicting": ring.evicting,
                })),
            })
        })
        .collect();
    channels.sort_by_key(|channel| channel["channel_id"].as_u64().unwrap_or_default());
    serde_json::json!({
        "captured_at_ms": report.captured_at.as_millis() as u64,
        "channels": channels,
        "over_budget": report.over_budget().len(),
        "evicting": report.evicting(),
    })
}

/// The reports this worker serves from, as one value.
pub type Reports = Arc<dyn DiagnosticReports>;
