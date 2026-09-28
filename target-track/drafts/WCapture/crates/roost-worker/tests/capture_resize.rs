//! Resize boundaries in a recording: an accepted resize records the
//! KEEPER-acknowledged parse boundary and the epoch transition, a lost ACK
//! leaves a record with no proven boundary and a coverage reason, and a capture
//! during an unresolved boundary reads the frozen core directly. Ports
//! `apps/worker/tests/terminal/terminal-capture-resize.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use capture_support::{CaptureHarness, capture_command, read_bundle};
use roost_protocol::terminal_capture::TerminalCaptureStatus;
use roost_protocol::terminal_capture::bundle::TerminalWorkerResizeOutcome as Outcome;
use roost_protocol::viewport::TerminalGeometry;
use roost_worker::browser_commands::diagnostics::DiagnosticReports;
use roost_worker::capture::tap::ResizeBoundaryNote;
use roost_worker::session::terminal_state::WorkerStreamResult;
use serde_json::json;
use terminal_stream_support::{COLS, ROWS, SESSION, STREAM_A, STREAM_B, channel, held};

#[tokio::test(flavor = "multi_thread")]
async fn an_accepted_resize_records_the_keeper_acknowledged_boundary_offset() {
    let harness = CaptureHarness::new("resize-accepted");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    assert_eq!(harness.start().status, TerminalCaptureStatus::Recording);
    let epoch_before = harness.with_record(|record| record.cell_emit.grid_epoch_base.clone());

    let resized = harness.stream.enable(STREAM_B, 10, 3).await;
    assert!(
        matches!(resized, WorkerStreamResult::Committed { resized: true, .. }),
        "{resized:?}"
    );

    let head_seq = harness.with_record(|record| record.head_seq);
    let resize = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            assert_eq!(recorder.resizes.len(), 1);
            recorder.resizes[0].clone()
        });
    assert_eq!(resize.outcome, Outcome::Accepted);
    // The boundary is the raw offset the ACK landed at: with no output in
    // flight it equals the install offset, and it is never null.
    assert_eq!(
        resize.boundary_offset.as_deref(),
        Some(head_seq.to_string().as_str())
    );
    assert_eq!(
        resize.boundary_offset.as_deref(),
        Some(resize.install_offset.as_str())
    );
    assert_eq!(
        resize.from,
        TerminalGeometry {
            cols: u32::from(COLS),
            rows: u32::from(ROWS)
        }
    );
    assert_eq!(resize.to, TerminalGeometry { cols: 10, rows: 3 });
    assert!(resize.grid_epoch_before.starts_with(&epoch_before));
    assert_ne!(
        resize.grid_epoch_after.as_deref(),
        Some(resize.grid_epoch_before.as_str())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_ack_retains_the_record_with_no_proven_boundary() {
    let harness = CaptureHarness::new("resize-lost");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.start();
    let tap = harness.recorder.tap();
    harness.with_record(|record| {
        let note = ResizeBoundaryNote {
            resize_seq: 11,
            install_seq: record.head_seq,
            from: (COLS, ROWS),
            to: (10, 3),
        };
        tap.resize_install(record, &note);
        tap.resize_result(record, &note, Outcome::LostAck, 24, None);
    });
    let (outcome, boundary) = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let resize = &recorder.unwrap().resizes[0];
            (resize.outcome, resize.boundary_offset.clone())
        });
    assert_eq!((outcome, boundary), (Outcome::LostAck, None));

    let result = harness.recorder.capture(capture_command()).await;
    assert_eq!(result.status, TerminalCaptureStatus::Partial);
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    assert_eq!(bundle["coverage"]["core_replay"], json!("partial"));
    assert!(
        bundle["coverage"]["core_replay_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("missing_resize_boundary"))
    );
    assert_eq!(bundle["worker"]["resizes"][0]["outcome"], json!("lost_ack"));
}

/// The capture never reaches for keeper history (it holds no keeper handle at
/// all), so it cannot contend with a lost-ACK recovery for the history slot:
/// what is pinned here is that the frozen core still answers its own reads.
#[tokio::test(flavor = "multi_thread")]
async fn a_capture_during_an_unresolved_resize_reads_the_frozen_core() {
    let harness = CaptureHarness::new("resize-open");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.start();
    harness.deliver(b"before-resize\r\n");
    // Open the gate and leave it unresolved: the core is frozen.
    assert!(held(&harness.stream.delivery).freeze_capture(channel(), 1_700_000_000_000));
    let tap = harness.recorder.tap();
    harness.with_record(|record| {
        let note = ResizeBoundaryNote {
            resize_seq: 7,
            install_seq: record.head_seq,
            from: (COLS, ROWS),
            to: (10, 3),
        };
        tap.resize_install(record, &note);
    });

    let result = harness.recorder.capture(capture_command()).await;
    assert_eq!(result.status, TerminalCaptureStatus::Partial);
    let worker = read_bundle(result.path.as_deref().unwrap()).await["worker"].clone();
    assert_eq!(worker["geometry"], json!({ "cols": COLS, "rows": ROWS }));
    assert_eq!(worker["resizes"][0]["boundary_offset"], json!(null));
}
