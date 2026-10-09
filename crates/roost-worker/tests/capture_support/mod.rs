//! The real capture recorder over the terminal-stream harness: its tap on the
//! harness's emitter, its bundles under a scratch log directory, and the wire
//! payloads remote layers actually send. Mirrors v2 `apps/worker/tests/
//! terminal/terminal-capture-fixtures.ts`; every `capture_*` test uses it.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use roost_protocol::cell::{CellGridFrame, CellRow, CellSpan, DEFAULT_COLOR};
use roost_protocol::terminal_capture::TerminalCaptureWorkerAck;
use roost_protocol::terminal_capture::bundle::is_terminal_capture_file_name;
use roost_protocol::wire::brand::SessionId;
use roost_term::{RioCore, grid_to_cell_frame, scrollback_origin};
use roost_worker::browser_commands::diagnostics::{
    CaptureAction, CaptureCommand, DiagnosticReports,
};
use roost_worker::capture::worker_section::WorkerProcessIdentity;
use roost_worker::capture::{CaptureRecorder, CaptureRecorderDeps};
use roost_worker::session::types::SessionRecord;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt as _;

use super::terminal_stream_support::{COLS, Harness, ROWS, SESSION, STREAM_A};

pub const RECORDING_ID: &str = "cccccccc-0000-4000-8000-00000000cccc";
pub const RIVAL_RECORDING_ID: &str = "cccccccc-0000-4000-8000-00000000cc99";
pub const CAPTURE_ID: &str = "eeeeeeee-0000-4000-8000-00000000eeee";
pub const AT_MS: u64 = 1_700_000_000_000;
pub const WORKER_FP: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A directory this test owns.
pub fn scratch(label: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!(
        "roost-capture-{label}-{}-{unique}",
        std::process::id()
    ))
}

pub fn process_identity() -> WorkerProcessIdentity {
    WorkerProcessIdentity {
        process_id: "dddddddd-0000-4000-8000-00000000dddd".to_owned(),
        git_sha: "test-sha".to_owned(),
        artifact_version: "test".to_owned(),
        worker_fp: WORKER_FP.to_owned(),
    }
}

/// The stream harness with the ONE recorder the worker would build over it.
pub struct CaptureHarness {
    pub stream: Harness,
    pub recorder: Arc<CaptureRecorder>,
    /// The worker log directory; it does not exist until the first write.
    pub log_dir: PathBuf,
    root: PathBuf,
}

impl Drop for CaptureHarness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl CaptureHarness {
    pub fn new(label: &str) -> Self {
        Self::over(label, Harness::scripted(RioCore::new(COLS, ROWS)))
    }

    pub fn over(label: &str, stream: Harness) -> Self {
        let root = scratch(label);
        let log_dir = root.join("RoostWorker");
        let recorder = CaptureRecorder::attach_to_emitter(
            CaptureRecorderDeps {
                table: Arc::clone(&stream.table),
                manager: Arc::clone(&stream.manager),
                log_dir: log_dir.clone(),
                process: process_identity(),
                runtime: tokio::runtime::Handle::current(),
            },
            &stream.emitter,
        );
        Self {
            stream,
            recorder,
            log_dir,
            root,
        }
    }

    pub fn start(&self) -> TerminalCaptureWorkerAck {
        self.recorder.start_recording(command(
            CaptureAction::Start,
            RECORDING_ID,
            "eeeeeeee-0000-4000-8000-00000000ee01",
        ))
    }

    pub async fn capture(&self, command: CaptureCommand) -> TerminalCaptureWorkerAck {
        self.recorder.capture(command).await
    }

    /// Paint rows into the core as an application would.
    pub fn paint(&self, rows: &[&str]) {
        self.stream.with_record(|record| {
            for (index, text) in rows.iter().enumerate() {
                record
                    .terminal_core
                    .write(format!("\x1b[{};1H{text}", index + 1).as_bytes());
            }
        });
    }

    /// Bytes delivered the way the keeper's dispatch thread delivers them.
    pub fn deliver(&self, bytes: &[u8]) {
        self.stream.deliver(bytes);
    }

    pub fn captured_files(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.log_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| is_terminal_capture_file_name(name))
            .collect()
    }

    pub fn with_record<R>(&self, read: impl FnOnce(&mut SessionRecord) -> R) -> R {
        self.stream.with_record(read)
    }
}

pub fn command(action: CaptureAction, recording_id: &str, capture_id: &str) -> CaptureCommand {
    CaptureCommand {
        action,
        session_id: SessionId::try_from(SESSION).unwrap(),
        recording_id: recording_id.to_owned(),
        capture_id: capture_id.to_owned(),
        reason: "manual".to_owned(),
        browser_evidence_json: String::new(),
        coordinator_evidence_json: String::new(),
    }
}

pub fn capture_command() -> CaptureCommand {
    command(CaptureAction::Capture, RECORDING_ID, CAPTURE_ID)
}

/// A dense viewport-only full exactly as the live emitter would produce it.
pub fn live_full_frame(record: &SessionRecord, seq: u64) -> CellGridFrame {
    let core = record.terminal_core.as_ref();
    let origin = scrollback_origin(core, record.cell_emit.scrollback_origin).unwrap();
    grid_to_cell_frame(
        core,
        seq,
        &record.cell_emit.grid_epoch(),
        STREAM_A,
        Some(0),
        origin,
    )
}

/// A full whose top row disagrees with the core at the same painted width:
/// "the grid the worker shipped is not the grid the core holds".
pub fn mismatched_full_frame(record: &SessionRecord, seq: u64) -> CellGridFrame {
    let mut frame = live_full_frame(record, seq);
    let span = CellSpan {
        text: "FOOTER-14s".to_owned(),
        fg: DEFAULT_COLOR,
        bg: DEFAULT_COLOR,
        flags: 0,
        fg_rgb: None,
        bg_rgb: None,
        columns: 10,
        link_uri: None,
        link_key: None,
    };
    frame.viewport_rows[0] = CellRow {
        index: 0,
        mark: 0,
        spans: vec![span].into(),
    };
    frame
}

pub async fn read_bundle(path: &str) -> Value {
    let compressed = tokio::fs::read(Path::new(path)).await.unwrap();
    let mut decoder = async_compression::tokio::bufread::GzipDecoder::new(compressed.as_slice());
    let mut json = String::new();
    decoder.read_to_string(&mut json).await.unwrap();
    serde_json::from_str(&json).unwrap()
}

fn envelope(layer: &str, capture_id: &str) -> Value {
    json!({ "schema": "roost.terminal-incident.v1", "layer": layer, "capture_id": capture_id, "recording_id": RECORDING_ID, "session_id": SESSION })
}

fn header(layer: &str, viewer: Option<&str>) -> serde_json::Map<String, Value> {
    json!({
        "layer": layer,
        "captured_at_ms": AT_MS,
        "process": {
            "layer": layer, "process_id": "ffffffff-0000-4000-8000-00000000ffff", "git_sha": "test-sha",
            "artifact_version": "test", "wasm_identity": null, "worker_fp": null,
            "viewer_id": viewer, "user_agent": viewer.map(|_| "test"),
        },
        "stream": null,
        "geometry": null,
        "dropped": { "records": 0, "bytes": 0, "rows": 0, "raw_bytes": 0, "samples": 0 },
        "omissions": [],
    })
    .as_object()
    .cloned()
    .unwrap()
}

/// The real browser payload: envelope + trigger, the SECTION nested under
/// `browser`, painting `dom_history` rows and `gaps` (start, end) ranges.
pub fn browser_payload(capture_id: &str, dom_history: &[u64], gaps: &[(String, String)]) -> Value {
    let rows: Vec<Value> = dom_history
        .iter()
        .map(|index| json!({ "order": index, "index": index, "columns": 10, "fingerprint": 0, "text": "", "span_count": 0 }))
        .collect();
    let gaps: Vec<Value> =
        gaps.iter().map(|(start, end)| json!({ "start": start, "end": end, "status": "unavailable", "rows": 0 })).collect();
    let mut section = header("browser", Some("viewer-1"));
    section.insert("events".to_owned(), json!([]));
    section.insert("replica".to_owned(), Value::Null);
    section.insert(
        "trigger_state".to_owned(),
        json!({
            "at_ms": AT_MS, "phase": "pre_destructive", "apply_mode": "full", "canonical": null,
            "committed": null, "pending": null, "painted_model_history": [], "dom_history": rows,
            "dom_viewport": [], "gaps": gaps, "cursor": null, "scroll": null, "reader": null,
            "active": true, "visible": true, "omissions": [],
        }),
    );
    for state in ["pre_repair_state", "post_repair_state", "current_state"] {
        section.insert(state.to_owned(), Value::Null);
    }
    let mut payload = envelope("browser", capture_id);
    payload["trigger"] = json!({
        "reason": "history_identity", "origin": "browser", "at_ms": AT_MS, "stream_id": STREAM_A,
        "grid_epoch": "assembly:0", "seq": "7", "detail": "duplicate_history_index", "occurrence_count": 3,
    });
    payload["browser"] = Value::Object(section);
    payload
}

/// The coordinator's payload, nested under ITS layer's member.
pub fn coordinator_payload(capture_id: &str) -> Value {
    let mut section = header("coordinator", None);
    section.insert("process".to_owned(), json!({
        "layer": "coordinator", "process_id": "aaaaaaaa-0000-4000-8000-00000000ab01", "git_sha": "test-sha",
        "artifact_version": "test", "wasm_identity": null, "worker_fp": null, "viewer_id": null, "user_agent": null,
    }));
    section.insert("records".to_owned(), json!([]));
    section.insert("snapshot".to_owned(), Value::Null);
    section.insert("valid".to_owned(), json!(true));
    let mut payload = envelope("coordinator", capture_id);
    payload["coordinator"] = Value::Object(section);
    payload
}

/// Enveloped and nested correctly, but the section violates the bundle gate.
pub fn malformed_section_payload(capture_id: &str) -> Value {
    let mut payload = browser_payload(capture_id, &[], &[]);
    payload["browser"]["events"] = json!("not-an-array");
    payload
}

/// A sound section shipped with a structurally invalid peer trigger.
pub fn invalid_trigger_payload(capture_id: &str) -> Value {
    let mut payload = browser_payload(capture_id, &[], &[]);
    payload["trigger"] = json!({});
    payload
}

/// The production shape of the defect: the section's fields flattened onto
/// the envelope, with no member named for the layer.
pub fn flattened_browser_payload(capture_id: &str) -> Value {
    let payload = browser_payload(capture_id, &[], &[]);
    let mut flattened = payload.as_object().cloned().unwrap();
    let section = flattened.remove("browser").unwrap();
    for (key, value) in section.as_object().unwrap() {
        flattened.insert(key.clone(), value.clone());
    }
    Value::Object(flattened)
}
