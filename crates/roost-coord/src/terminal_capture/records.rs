//! The coordinator layer's evidence shapes: one record per accepted full or
//! folded delta, the section a capture freezes them into, and the envelope
//! that nests the section under its layer name. Serialized exactly as v2's
//! `JSON.stringify` spelled them. Ports `TerminalCoordinatorRecord` and
//! `TerminalCoordinatorSection` of `packages/protocol/src/terminal-capture-bundle.ts`
//! and `TerminalCaptureCoordinatorPayload` of `terminal-capture.ts`, which
//! `roost_protocol::terminal_capture` does not carry; every member type it does
//! carry is reused. Used by `terminal_capture::{recorder, freeze}`.

use std::sync::Arc;

use roost_protocol::cell::CellGridFrame;
use roost_protocol::terminal_capture::bundle::{
    TerminalCaptureDropCounters, TerminalCaptureLayer, TerminalCaptureOmission,
    TerminalCaptureProcessIdentity, TerminalCaptureStreamIdentity,
};
use roost_protocol::terminal_capture::frame_json::FrameJson;
use roost_protocol::viewport::TerminalGeometry;
use serde::{Serialize, Serializer};

/// Just enough of the admitted wire frame to name what the hub accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmittedFrameIdentity {
    pub full: bool,
    pub seq: u64,
    pub base_seq: u64,
}

/// Whether the hub's snapshot was installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorSnapshotState {
    Installed,
}

/// Whether the frame reached a watcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorSendState {
    Queued,
    NotSent,
}

/// What the coordinator did about a sequence gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinatorRepair {
    None,
    RequestedFull,
}

/// A full that did not continue the retained fold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SequenceGap {
    pub from: String,
    pub to: String,
}

/// One accepted frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerminalCoordinatorRecord {
    pub at_ms: i64,
    pub stream: TerminalCaptureStreamIdentity,
    pub admitted_full: bool,
    pub accepted: bool,
    /// The canonical viewport AFTER admission; `None` when over budget.
    #[serde(serialize_with = "serialize_canonical")]
    pub canonical: Option<Arc<CellGridFrame>>,
    pub snapshot_state: CoordinatorSnapshotState,
    pub send_state: CoordinatorSendState,
    pub gap: Option<SequenceGap>,
    pub repair: CoordinatorRepair,
}

/// The layer's frozen evidence.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerminalCoordinatorSection {
    pub layer: TerminalCaptureLayer,
    pub captured_at_ms: i64,
    pub process: TerminalCaptureProcessIdentity,
    pub stream: Option<TerminalCaptureStreamIdentity>,
    pub geometry: Option<TerminalGeometry>,
    pub dropped: TerminalCaptureDropCounters,
    pub omissions: Vec<TerminalCaptureOmission>,
    pub records: Vec<TerminalCoordinatorRecord>,
    pub snapshot: Option<TerminalCaptureStreamIdentity>,
    pub valid: bool,
}

/// The envelope: capture identity, and the section NESTED under `coordinator`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerminalCaptureCoordinatorPayload<'a> {
    pub schema: &'static str,
    pub layer: TerminalCaptureLayer,
    pub capture_id: &'a str,
    pub recording_id: &'a str,
    pub session_id: &'a str,
    pub coordinator: TerminalCoordinatorSection,
}

fn serialize_canonical<S: Serializer>(
    frame: &Option<Arc<CellGridFrame>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match frame {
        Some(frame) => FrameJson(frame).serialize(serializer),
        None => serializer.serialize_none(),
    }
}
