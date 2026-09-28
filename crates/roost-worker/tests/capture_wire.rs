//! Capture wiring: the REAL emitter feeds the recorder at accepted emissions
//! only (a healthy full+delta compares equal and stays silent, a dropped delta
//! invalidates the fold), and the `diag-terminal-capture` frame is answered
//! rpc-ok with the worker ack for start, capture and stop — a fixed code, never
//! a message. Ports `apps/worker/tests/terminal/terminal-capture-wire.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use std::sync::{Arc, Mutex};

use capture_support::{CaptureHarness, RECORDING_ID, scratch};
use roost_protocol::cell::CellGridFrame;
use roost_protocol::cell::frame_chunks::CellGridSnapshotPart;
use roost_protocol::terminal_capture::bundle::{TerminalCoverageReason, TerminalWorkerComparison};
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use roost_worker::browser_commands::search::Searches;
use roost_worker::browser_commands::{Command, dispatch};
use roost_worker::runtime::deps::WorkerCapabilities;
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};
use serde_json::{Value, json};
use terminal_stream_support::{COLS, ROWS, SESSION, STREAM_A, held};

const FINGERPRINT: &str = "0000000000000000000000000000000000000000000000000000000000000000";

fn emit(harness: &CaptureHarness, force: bool) {
    let emitter = Arc::clone(&harness.stream.emitter);
    harness.with_record(|record| {
        held(&emitter).emit_cell_frame(record, force, 1_700_000_000_000);
    });
}

fn emissions(harness: &CaptureHarness) -> Vec<(bool, TerminalWorkerComparison)> {
    harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            recorder
                .unwrap()
                .emissions
                .iter()
                .map(|entry| (entry.record.full, entry.record.comparison))
                .collect()
        })
}

#[tokio::test(flavor = "multi_thread")]
async fn the_live_emitter_records_accepted_frames_and_stays_silent_when_healthy() {
    let harness = CaptureHarness::new("wire-healthy");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.paint(&["FOOTER-12s"]);
    harness.start();

    emit(&harness, true);
    // Force the second emission to be sampled too, so "silent" is not just
    // "unsampled".
    harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            recorder.unwrap().last_sample_mono = None
        });
    harness.with_record(|record| record.terminal_core.write(b"\x1b[2;1HFOOTER-14s"));
    emit(&harness, false);
    harness.recorder.settle_scheduled_captures().await;

    assert_eq!(
        emissions(&harness),
        vec![
            (true, TerminalWorkerComparison::Equal),
            (false, TerminalWorkerComparison::Equal)
        ]
    );
    let (folded, sampled) = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            (recorder.fold.is_some(), recorder.sampling.sampled)
        });
    assert_eq!((folded, sampled), (true, 2));
    assert!(harness.captured_files().is_empty());
}

/// The coordinator sink, dropping every delta and taking every full.
struct DeltaDroppingSink;

impl CellSink for DeltaDroppingSink {
    fn id(&self) -> &str {
        "coord"
    }
    fn send_frame(
        &self,
        _channel_id: ChannelId,
        frame: &CellGridFrame,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        if frame.full {
            CellSinkResult::Sent
        } else {
            CellSinkResult::Dropped
        }
    }
    fn send_snapshot_part(
        &self,
        _channel_id: ChannelId,
        _part: &CellGridSnapshotPart,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        CellSinkResult::Sent
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_delta_on_the_live_path_invalidates_the_fold() {
    let harness = CaptureHarness::new("wire-dropped");
    {
        let mut emitter = held(&harness.stream.emitter);
        emitter.unregister_sink("coord");
        emitter.register_sink(Arc::new(DeltaDroppingSink));
    }
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.start();
    emit(&harness, true);
    assert!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.unwrap().fold.is_some())
    );

    harness.with_record(|record| record.terminal_core.write(b"\x1b[3;1Hdropped"));
    emit(&harness, false);
    // The dropped delta was never retained as evidence, and the repair full
    // the emitter then built re-established the baseline it invalidated.
    let reasons = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| recorder.unwrap().fold_reasons.clone());
    assert!(reasons.contains(&TerminalCoverageReason::BaselineInvalidated));
    assert!(emissions(&harness).iter().all(|(full, _)| *full));
}

/// The ONE production `Deps`, with the harness's recorder as its diagnostics.
fn production_deps(harness: &CaptureHarness) -> roost_worker::browser_commands::Deps {
    WorkerCapabilities {
        sessions: Arc::clone(&harness.stream.table),
        manager: Arc::clone(&harness.stream.manager),
        attachment_root: scratch("wire-attachments"),
        capture: Arc::clone(&harness.recorder),
        platform: roost_host::HostPlatform::Linux,
        searches: Arc::new(Mutex::new(Searches::default())),
    }
    .into_deps()
}

fn frame(action: &str, browser_evidence_json: &str) -> Command {
    let value = json!({
        "kind": "diag-terminal-capture",
        "request_id": "req-1",
        "session_id": SESSION,
        "recording_id": RECORDING_ID,
        "capture_id": "eeeeeeee-0000-4000-8000-00000000ee04",
        "action": action,
        "reason": "manual",
        "browser_evidence_json": browser_evidence_json,
        "coordinator_evidence_json": "",
    });
    Command::decode(FINGERPRINT, FINGERPRINT, "req-1", value)
        .expect("a canonical capture frame decodes")
}

async fn answer(deps: &roost_worker::browser_commands::Deps, command: Command) -> Value {
    let mut frames = dispatch(&command, deps).await;
    assert_eq!(frames.len(), 1, "a capture step is answered exactly once");
    match frames.remove(0) {
        CoordWorkerUpstream::RpcOk { data, .. } => data,
        other => panic!("a capture step answers rpc-ok, not {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_capture_frame_is_answered_rpc_ok_for_start_capture_and_stop() {
    let harness = CaptureHarness::new("wire-frames");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    let deps = production_deps(&harness);

    let started = answer(&deps, frame("start", "")).await;
    assert_eq!(
        (started["status"].clone(), started["error"].clone()),
        (json!("recording"), Value::Null)
    );

    let captured = answer(&deps, frame("capture", "")).await;
    assert_eq!(
        (captured["status"].clone(), captured["error"].clone()),
        (json!("partial"), Value::Null)
    );
    let path = captured["path"].as_str().unwrap();
    assert!(
        path.contains("terminal-incident-") && path.len() <= 1024,
        "{path}"
    );

    let stopped = answer(&deps, frame("stop", "")).await;
    assert_eq!(
        (stopped["status"].clone(), stopped["error"].clone()),
        (json!("stopped"), Value::Null)
    );
    assert!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.is_none())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_browser_evidence_is_refused_with_a_fixed_code() {
    let harness = CaptureHarness::new("wire-malformed");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    let refused = answer(&production_deps(&harness), frame("capture", "{not json")).await;
    assert_eq!(
        (
            refused["status"].clone(),
            refused["error"].clone(),
            refused["path"].clone()
        ),
        (json!("error"), json!("evidence_malformed"), Value::Null)
    );
}
