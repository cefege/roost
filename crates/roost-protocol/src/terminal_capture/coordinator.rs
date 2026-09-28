//! The coordinator layer's evidence shapes: one record per accepted full or
//! folded delta, the section a capture freezes them into, and the envelope
//! that nests the section under its layer name, serialized as v2's
//! `JSON.stringify` spells them. Ports `TerminalCoordinatorRecord` and
//! `TerminalCoordinatorSection` of `packages/protocol/src/terminal-capture-bundle.ts`
//! and `TerminalCaptureCoordinatorPayload` of `terminal-capture.ts`. Produced
//! by the coordinator's capture recorder (`roost_coord::terminal_capture`);
//! checked on the way into a bundle by `validate`.
//!
//! The state enums carry only the values the coordinator records: v2's wider
//! unions (`installing`, `dropped`, `invalidated`, ...) have no producer.

use std::sync::Arc;

use serde::{Serialize, Serializer};

use super::bundle::{
    TerminalCaptureDropCounters, TerminalCaptureLayer, TerminalCaptureOmission,
    TerminalCaptureProcessIdentity, TerminalCaptureStreamIdentity,
};
use super::frame_json::FrameJson;
use crate::cell::CellGridFrame;
use crate::viewport::TerminalGeometry;

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

/// A full that did not continue the retained fold: decimal sequence bounds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SequenceGap {
    pub from: String,
    pub to: String,
}

/// One accepted frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerminalCoordinatorRecord {
    pub at_ms: u64,
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

/// The layer's frozen evidence. `layer` always serializes as `"coordinator"`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TerminalCoordinatorSection {
    pub layer: TerminalCaptureLayer,
    pub captured_at_ms: u64,
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
