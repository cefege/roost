//! The worker's terminal-incident recorder: one file per repeating mismatch
//! (and no second one for a fresh epoch inside the automatic floor), a
//! healthy stream stays silent, a dropped delta invalidates the fold, lease
//! expiry and session close free everything, and eviction makes replay
//! explicitly partial. Ports `apps/worker/tests/terminal/
//! terminal-capture-recorder.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod capture_support;
mod terminal_stream_support;

use capture_support::{
    CaptureHarness, RECORDING_ID, RIVAL_RECORDING_ID, capture_command, command, live_full_frame,
    mismatched_full_frame, read_bundle,
};
use roost_protocol::terminal_capture::bundle::{TerminalCoverageReason, TerminalWorkerComparison};
use roost_protocol::terminal_capture::{
    TERMINAL_CAPTURE_LIMITS, TerminalCaptureErrorCode, TerminalCaptureStatus,
};
use roost_worker::browser_commands::diagnostics::{CaptureAction, DiagnosticReports};
use roost_worker::capture::now_ms;
use serde_json::json;
use terminal_stream_support::{CHANNEL, COLS, ROWS, SESSION, STREAM_A};

async fn armed(label: &str) -> CaptureHarness {
    let harness = CaptureHarness::new(label);
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.paint(&["FOOTER-12s", "row-1", "row-2"]);
    assert_eq!(harness.start().status, TerminalCaptureStatus::Recording);
    harness
}

fn tap_mismatch(harness: &CaptureHarness, seq: u64) {
    let tap = harness.recorder.tap();
    harness.with_record(|record| {
        tap.accepted_emission(record, Some(mismatched_full_frame(record, seq)))
    });
}

fn tap_full(harness: &CaptureHarness, seq: u64) {
    let tap = harness.recorder.tap();
    harness.with_record(|record| tap.accepted_emission(record, Some(live_full_frame(record, seq))));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeating_same_generation_mismatch_writes_exactly_one_file() {
    let harness = armed("latch").await;
    tap_mismatch(&harness, 1);
    harness.recorder.settle_scheduled_captures().await;
    assert_eq!(harness.captured_files().len(), 1);

    // Isolate the LATCH: clear the session-wide floor and the sample interval
    // so nothing but the per-identity latch can suppress the second capture.
    harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            (recorder.last_automatic_ms, recorder.last_sample_mono) = (None, None);
        });
    tap_mismatch(&harness, 2);
    harness.recorder.settle_scheduled_captures().await;
    assert_eq!(harness.captured_files().len(), 1);
    let occurrences = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            recorder.occurrences[&recorder.latches[0]]
        });
    assert_eq!(occurrences, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_grid_epoch_does_not_bypass_the_session_wide_automatic_floor() {
    let harness = armed("floor").await;
    tap_mismatch(&harness, 1);
    harness.recorder.settle_scheduled_captures().await;
    assert_eq!(harness.captured_files().len(), 1);

    // A fresh epoch is a fresh latch key, so only the 60 s floor can stop it.
    harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            recorder.unwrap().last_sample_mono = None
        });
    harness.with_record(|record| record.cell_emit.grid_epoch_revision += 1);
    tap_mismatch(&harness, 2);
    harness.recorder.settle_scheduled_captures().await;
    assert_eq!(harness.captured_files().len(), 1);
    assert_eq!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.unwrap().latches.len()),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_healthy_stream_stays_silent_and_is_sampled_every_time() {
    let harness = armed("healthy").await;
    tap_full(&harness, 1);
    // Clear BOTH sampling gates: the interval, and the budget suppression a
    // slow first scan on a loaded machine would otherwise leave armed.
    harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            (
                recorder.last_sample_mono,
                recorder.sample_suppressed_until_mono,
            ) = (None, None);
        });
    harness.with_record(|record| record.terminal_core.write(b"\x1b[3;1Hnext"));
    tap_full(&harness, 2);
    harness.recorder.settle_scheduled_captures().await;

    assert!(harness.captured_files().is_empty());
    let (sampled, comparisons) = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            let comparisons: Vec<_> = recorder
                .emissions
                .iter()
                .map(|entry| entry.record.comparison)
                .collect();
            (recorder.sampling.sampled, comparisons)
        });
    assert_eq!(sampled, 2);
    assert_eq!(
        comparisons,
        vec![
            TerminalWorkerComparison::Equal,
            TerminalWorkerComparison::Equal
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_delta_invalidates_the_fold_until_the_next_accepted_full() {
    let harness = armed("dropped").await;
    let tap = harness.recorder.tap();
    let fold_held = || {
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.unwrap().fold.is_some())
    };
    tap_full(&harness, 1);
    assert!(fold_held());

    harness.with_record(|record| {
        tap.rejected_emission(record, TerminalCoverageReason::BaselineInvalidated)
    });
    assert!(!fold_held());
    let reasons = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| recorder.unwrap().fold_reasons.clone());
    assert!(reasons.contains(&TerminalCoverageReason::BaselineInvalidated));

    // A sparse delta cannot rebuild a baseline it no longer has.
    harness.with_record(|record| {
        let mut delta = live_full_frame(record, 2);
        (delta.full, delta.base_seq) = (false, 1);
        delta.viewport_rows.truncate(1);
        tap.accepted_emission(record, Some(delta));
    });
    assert!(!fold_held());
    let last = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            recorder
                .unwrap()
                .emissions
                .back()
                .unwrap()
                .record
                .comparison
        });
    assert_eq!(last, TerminalWorkerComparison::BaselineInvalid);

    tap_full(&harness, 3);
    assert!(fold_held());
}

#[tokio::test(flavor = "multi_thread")]
async fn lease_expiry_disarms_the_recorder_and_frees_its_records() {
    let harness = armed("expiry").await;
    tap_full(&harness, 1);
    let emissions = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            let recorder = recorder.unwrap();
            recorder.expires_at_ms = now_ms() - 1;
            recorder.emissions.len()
        });
    assert_eq!(emissions, 1);
    assert!(!harness.recorder._terminal_recorder_armed(SESSION));
    assert!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.is_none())
    );

    // A later tap on the same session allocates nothing.
    let tap = harness.recorder.tap();
    harness.with_record(|record| tap.retain_output(record, 8, &[0; 8]));
    assert!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.is_none())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn session_close_frees_the_recorder() {
    let harness = armed("close").await;
    assert!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.is_some())
    );
    // v2 `_dropChannelState`: the close notifies the session-closed hooks.
    let _ = harness.stream.manager.close_channel(CHANNEL, Some(0)).await;
    assert!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.is_none())
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_recording_on_a_live_lease_conflicts_instead_of_evicting_it() {
    let harness = armed("conflict").await;
    let rival = harness.recorder.start_recording(command(
        CaptureAction::Start,
        RIVAL_RECORDING_ID,
        "eeeeeeee-0000-4000-8000-00000000ee02",
    ));
    assert_eq!(
        (rival.status, rival.error),
        (
            TerminalCaptureStatus::Error,
            Some(TerminalCaptureErrorCode::LeaseConflict)
        )
    );
    let held_by = harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| recorder.unwrap().recording_id.clone());
    assert_eq!(held_by, RECORDING_ID);

    // STOP from another recording is denied; the owner's is not, and a repeat
    // by the owner is harmless.
    let denied = harness.recorder.stop_recording(command(
        CaptureAction::Stop,
        RIVAL_RECORDING_ID,
        "eeeeeeee-0000-4000-8000-00000000ee03",
    ));
    assert_eq!(
        denied.error,
        Some(TerminalCaptureErrorCode::PermissionDenied)
    );
    for _ in 0..2 {
        let stopped = harness.recorder.stop_recording(command(
            CaptureAction::Stop,
            RECORDING_ID,
            "eeeeeeee-0000-4000-8000-00000000ee04",
        ));
        assert_eq!(stopped.status, TerminalCaptureStatus::Stopped);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renewing_start_preserves_retained_evidence() {
    let harness = armed("renew").await;
    tap_full(&harness, 1);
    harness
        .recorder
        ._with_terminal_recorder(SESSION, |recorder| {
            recorder.unwrap().expires_at_ms = now_ms() + 1_000
        });
    let renewed = harness.start();
    assert_eq!(renewed.status, TerminalCaptureStatus::Recording);
    assert!(renewed.expires_at_ms.unwrap() > now_ms() + 60_000);
    assert_eq!(
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| recorder.unwrap().emissions.len()),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn raw_cap_eviction_makes_core_replay_explicitly_partial_with_a_named_reason() {
    let harness = armed("raw-cap").await;
    let tap = harness.recorder.tap();
    let chunk = [0x41_u8; 16];
    let mut end_seq = 0;
    for _ in 0..TERMINAL_CAPTURE_LIMITS.layer_entries + 4 {
        end_seq += chunk.len() as u64;
        harness.with_record(|record| tap.retain_output(record, end_seq, &chunk));
    }
    let (retained, prefix_complete) =
        harness
            .recorder
            ._with_terminal_recorder(SESSION, |recorder| {
                let recorder = recorder.unwrap();
                (recorder.raw.len(), recorder.retention.raw_prefix_complete)
            });
    assert_eq!(
        (retained, prefix_complete),
        (TERMINAL_CAPTURE_LIMITS.layer_entries, false)
    );

    let result = harness.capture(capture_command()).await;
    assert_eq!(result.status, TerminalCaptureStatus::Partial);
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    assert_eq!(bundle["coverage"]["core_replay"], json!("partial"));
    assert!(
        bundle["coverage"]["core_replay_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("raw_prefix_evicted"))
    );
    let omissions = bundle["worker"]["omissions"].as_array().unwrap();
    let raw = omissions
        .iter()
        .find(|omission| omission["name"] == json!("worker.raw"))
        .unwrap();
    assert_eq!(raw["reason"], json!("raw_prefix_evicted"));
    assert_eq!(raw["dropped_count"], json!(4));
    assert_eq!(raw["range"], json!({ "start": "0", "end": "64" }));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unarmed_manual_capture_still_carries_the_legacy_raw_tail() {
    let harness = CaptureHarness::new("unarmed");
    harness.stream.enable(STREAM_A, COLS, ROWS).await;
    harness.deliver(b"hello-unarmed");

    let result = harness.capture(capture_command()).await;
    assert_eq!(result.status, TerminalCaptureStatus::Partial);
    let bundle = read_bundle(result.path.as_deref().unwrap()).await;
    assert_eq!(
        bundle["worker"]["byte_capture"]["byte_length"],
        json!("hello-unarmed".len())
    );
    assert_eq!(bundle["coverage"]["core_replay"], json!("unavailable"));
    assert_eq!(
        bundle["coverage"]["core_replay_reasons"],
        json!(["layer_unavailable"])
    );
    assert_eq!(
        (bundle["browser"].clone(), bundle["coordinator"].clone()),
        (json!(null), json!(null))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retried_capture_id_returns_the_original_result_and_writes_no_second_file() {
    let harness = armed("retry").await;
    let first = harness.capture(capture_command()).await;
    assert!(first.path.is_some());
    let retry = harness.capture(capture_command()).await;
    assert_eq!(retry, first);
    assert_eq!(harness.captured_files().len(), 1);
}
