//! Coordinator capture records: the screen hub's accepted-frame hook, gap and
//! send-state reporting, renewal versus a new recording, and the entry bound
//! that keeps the retained head a complete checkpoint.
//!
//! Ports the record cases of
//! `apps/coord/tests/terminal/capture/terminal-capture-recorder.test.ts`; its
//! freeze cases are `terminal_capture_freeze.rs`. Hub admission runs through
//! the real hub harness so the hook wiring is proven, not simulated.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_capture_support;
mod terminal_screen_hub_support;

use std::sync::Arc;

use roost_coord::terminal_capture::TerminalCaptureRuntime;
use roost_coord::terminal_capture::freeze::FreezeContext;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::coordinator::{
    CoordinatorRepair, CoordinatorSendState, CoordinatorSnapshotState, SequenceGap,
};
use terminal_capture_support::{
    CAPTURE_1, EPOCH, RECORDING_A, RECORDING_B, SESSION_A as SESSION, STREAM, admitted,
    canonical_frame, canonical_frame_in,
};
use terminal_screen_hub_support::{TestSink, baseline, delta, harness, session, watch};

const CONTEXT: FreezeContext<'static> = FreezeContext {
    git_sha: "test",
    captured_at_ms: 1,
};

fn runtime() -> Arc<TerminalCaptureRuntime> {
    Arc::new(TerminalCaptureRuntime::new())
}

fn texts(frame: &roost_protocol::cell::CellGridFrame) -> Vec<String> {
    frame
        .viewport_rows
        .iter()
        .map(|row| row.spans.iter().map(|span| span.text.as_str()).collect())
        .collect()
}

#[test]
fn an_unarmed_session_retains_nothing_and_reports_its_layer_unavailable() {
    let capture = runtime();
    let h = harness();
    h.hub.install_capture(Arc::clone(&capture));
    watch(&h.hub, &TestSink::queuing(), "socket-a");
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    h.frame(baseline(1, &["old-a", "old-b"]));
    h.frame(delta(1, "new-b"));

    let recorder = &capture.recorder;
    assert!(!recorder.armed(SESSION));
    assert_eq!(recorder.stats(SESSION), None);
    assert!(recorder.records(SESSION).is_empty());
    let evidence = recorder.freeze(SESSION, CAPTURE_1, RECORDING_A, CONTEXT);
    assert_eq!(
        (evidence.available, evidence.json.as_str(), evidence.records),
        (false, "", 0)
    );
}

#[test]
fn records_the_admitted_full_and_the_folded_delta_with_the_hubs_canonical() {
    let capture = runtime();
    let h = harness();
    h.hub.install_capture(Arc::clone(&capture));
    watch(&h.hub, &TestSink::queuing(), "socket-a");
    h.hub.expect_stream(&session(), STREAM, 8, 2);
    capture.recorder.arm(SESSION, RECORDING_A);

    h.frame(baseline(1, &["old-a", "old-b"]));
    h.frame(delta(1, "new-b"));

    let records = capture.recorder.records(SESSION);
    assert_eq!(records.len(), 2);
    let first = &records[0];
    assert!(first.admitted_full && first.accepted);
    assert_eq!(first.snapshot_state, CoordinatorSnapshotState::Installed);
    assert_eq!(first.send_state, CoordinatorSendState::Queued);
    assert_eq!(
        (first.gap.as_ref(), first.repair),
        (None, CoordinatorRepair::None)
    );
    let stream = &first.stream;
    assert_eq!(
        (
            stream.stream_id.as_str(),
            stream.grid_epoch.as_str(),
            stream.seq.as_str()
        ),
        (STREAM, EPOCH, "1")
    );
    assert_eq!(
        (stream.base_seq.as_deref(), stream.cols, stream.rows),
        (None, 8, 2)
    );
    let second = &records[1];
    assert!(!second.admitted_full && second.accepted);
    assert_eq!(
        (second.gap.as_ref(), second.repair),
        (None, CoordinatorRepair::None)
    );
    assert_eq!(
        (
            second.stream.seq.as_str(),
            second.stream.base_seq.as_deref()
        ),
        ("2", Some("1"))
    );
    // The retained canonical is the hub's own post-admission viewport, so the
    // folded row is visible without re-deriving the grid.
    let folded = second.canonical.as_ref().unwrap();
    assert!(folded.full);
    assert_eq!(texts(folded), ["old-a", "new-b"]);
    assert_eq!(texts(first.canonical.as_ref().unwrap())[1], "old-b");
}

#[test]
fn reports_a_repaired_baseline_as_a_sequence_gap_and_no_watcher_as_not_sent() {
    let capture = runtime();
    let recorder = &capture.recorder;
    recorder.arm(SESSION, RECORDING_A);
    recorder.record(
        SESSION,
        &canonical_frame(4, 2, 1, None),
        admitted(true, 4, 0),
        0,
        1,
    );
    // A fresh full that skips sequences is the coordinator asking the worker
    // for a new baseline after its own replica failed closed.
    recorder.record(
        SESSION,
        &canonical_frame(9, 2, 1, None),
        admitted(true, 9, 0),
        1,
        1,
    );

    let records = recorder.records(SESSION);
    assert_eq!(records[0].send_state, CoordinatorSendState::NotSent);
    assert_eq!(
        (records[0].gap.as_ref(), records[0].repair),
        (None, CoordinatorRepair::None)
    );
    assert_eq!(records[1].send_state, CoordinatorSendState::Queued);
    let gap = SequenceGap {
        from: "4".to_owned(),
        to: "9".to_owned(),
    };
    assert_eq!(
        (records[1].gap.as_ref(), records[1].repair),
        (Some(&gap), CoordinatorRepair::RequestedFull)
    );
    // A new epoch restarts the numbering by design and is not a gap.
    let restarted = canonical_frame_in(1, 2, 1, None, "grid-epoch-b", 80);
    recorder.record(SESSION, &restarted, admitted(true, 1, 0), 1, 1);
    let third = &recorder.records(SESSION)[2];
    assert_eq!(
        (third.gap.as_ref(), third.repair),
        (None, CoordinatorRepair::None)
    );
}

#[test]
fn a_renewal_keeps_evidence_and_a_different_recording_starts_empty() {
    let capture = runtime();
    let recorder = &capture.recorder;
    recorder.arm(SESSION, RECORDING_A);
    recorder.record(
        SESSION,
        &canonical_frame(1, 2, 1, None),
        admitted(true, 1, 0),
        0,
        1,
    );
    recorder.arm(SESSION, RECORDING_A);
    let stats = recorder.stats(SESSION).unwrap();
    assert_eq!(
        (stats.recording_id.as_str(), stats.records),
        (RECORDING_A, 1)
    );

    recorder.arm(SESSION, RECORDING_B);
    let stats = recorder.stats(SESSION).unwrap();
    assert_eq!(
        (stats.recording_id.as_str(), stats.records),
        (RECORDING_B, 0)
    );
    // Evidence belongs to one recording: another page's capture cannot read it.
    assert!(
        !recorder
            .freeze(SESSION, CAPTURE_1, RECORDING_A, CONTEXT)
            .available
    );
    assert_eq!(recorder.disarm(SESSION).records, 0);
    assert!(!recorder.armed(SESSION));
}

#[test]
fn entry_eviction_keeps_the_retained_head_a_complete_checkpoint() {
    let capture = runtime();
    let recorder = &capture.recorder;
    recorder.arm(SESSION, RECORDING_A);
    let total = TERMINAL_CAPTURE_LIMITS.layer_entries as u64 + 8;
    for seq in 1..=total {
        recorder.record(
            SESSION,
            &canonical_frame(seq, 2, 1, None),
            admitted(seq == 1, seq, seq - 1),
            0,
            1,
        );
    }
    let records = recorder.records(SESSION);
    assert_eq!(records.len(), TERMINAL_CAPTURE_LIMITS.layer_entries);
    assert!(records[0].canonical.is_some());
    assert_eq!(records.last().unwrap().stream.seq, total.to_string());
    assert_eq!(recorder.stats(SESSION).unwrap().dropped, 8);
}
