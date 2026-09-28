//! The reply validation boundary the worker-frame dispatcher applies before a
//! pending pipeline sample settles, and the JSON-safe projection of an accepted
//! reply.
//!
//! Ports the wire-shape bounds of `isTerminalPipelineSnapshotWireShape` and the
//! `terminalPipelineDiagnosticSnapshot` projection of
//! `apps/coord/src/terminal/screen/worker-terminal-pipeline-snapshot.ts`, which the
//! v2 suite (`worker-terminal-pipeline-snapshot.test.ts`, "response validation
//! boundary") reaches only through a collected reply.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_screen_pipeline_support;

use std::collections::HashSet;

use roost_coord::terminal_screen::pipeline_projection::terminal_pipeline_diagnostic_snapshot;
use roost_coord::terminal_screen::pipeline_snapshot::{
    TERMINAL_PIPELINE_DIAG_MAX_TARGETS, is_terminal_pipeline_snapshot_wire_shape,
};
use roost_proto::{TerminalPipelineReason, TerminalPipelineStage, WTerminalPipelineSnapshot};
use terminal_screen_pipeline_support::{pipeline_reply, target, worker_stream_stage};

fn well_shaped() -> WTerminalPipelineSnapshot {
    pipeline_reply("request-1", &[target("session-1", "view-1")])
}

#[test]
fn a_reply_with_only_a_request_id_is_well_shaped() {
    assert!(is_terminal_pipeline_snapshot_wire_shape(
        &WTerminalPipelineSnapshot {
            request_id: "request-1".to_owned(),
            ..Default::default()
        }
    ));
    assert!(is_terminal_pipeline_snapshot_wire_shape(&well_shaped()));
}

#[test]
fn oversized_identifiers_are_refused() {
    let mut reply = well_shaped();
    reply.request_id = "r".repeat(257);
    assert!(!is_terminal_pipeline_snapshot_wire_shape(&reply));
    reply.request_id = "r".repeat(256);
    assert!(is_terminal_pipeline_snapshot_wire_shape(&reply));

    for mutate in [
        |reply: &mut WTerminalPipelineSnapshot| reply.sessions[0].session_id = "s".repeat(513),
        |reply: &mut WTerminalPipelineSnapshot| reply.sessions[0].view_id = "v".repeat(513),
        |reply: &mut WTerminalPipelineSnapshot| {
            reply.sessions[0].stages[0].stream_id = "x".repeat(513)
        },
    ] {
        let mut reply = well_shaped();
        mutate(&mut reply);
        assert!(!is_terminal_pipeline_snapshot_wire_shape(&reply));
    }
    let mut at_bound = well_shaped();
    at_bound.sessions[0].session_id = "s".repeat(512);
    at_bound.sessions[0].view_id = "v".repeat(512);
    at_bound.sessions[0].stages[0].stream_id = "x".repeat(512);
    assert!(is_terminal_pipeline_snapshot_wire_shape(&at_bound));
}

#[test]
fn counts_past_their_bounds_are_refused() {
    let many: Vec<_> = (0..=TERMINAL_PIPELINE_DIAG_MAX_TARGETS)
        .map(|index| target(&format!("session-{index}"), ""))
        .collect();
    assert!(!is_terminal_pipeline_snapshot_wire_shape(&pipeline_reply(
        "request-1",
        &many
    )));
    assert!(is_terminal_pipeline_snapshot_wire_shape(&pipeline_reply(
        "request-1",
        &many[..TERMINAL_PIPELINE_DIAG_MAX_TARGETS]
    )));

    let mut stages = well_shaped();
    stages.sessions[0].stages = vec![worker_stream_stage(); 17];
    assert!(!is_terminal_pipeline_snapshot_wire_shape(&stages));
    stages.sessions[0].stages.pop();
    assert!(is_terminal_pipeline_snapshot_wire_shape(&stages));

    let mut buckets = well_shaped();
    buckets.sessions[0].stages[0].histogram_buckets = vec![1; 17];
    assert!(!is_terminal_pipeline_snapshot_wire_shape(&buckets));
    buckets.sessions[0].stages[0].histogram_buckets.pop();
    assert!(is_terminal_pipeline_snapshot_wire_shape(&buckets));
}

#[test]
fn a_duplicate_session_view_pair_is_refused() {
    let duplicate = pipeline_reply(
        "request-1",
        &[target("session-1", "view-1"), target("session-1", "view-1")],
    );
    assert!(!is_terminal_pipeline_snapshot_wire_shape(&duplicate));
    let distinct = pipeline_reply(
        "request-1",
        &[target("session-1", "view-1"), target("session-1", "view-2")],
    );
    assert!(is_terminal_pipeline_snapshot_wire_shape(&distinct));
}

#[test]
fn only_worker_stages_and_known_reasons_are_accepted() {
    let with_stage = |stage: i32| {
        let mut reply = well_shaped();
        reply.sessions[0].stages[0].stage = stage.into();
        is_terminal_pipeline_snapshot_wire_shape(&reply)
    };
    assert!(!with_stage(TerminalPipelineStage::Unspecified as i32));
    assert!(with_stage(TerminalPipelineStage::WorkerPty as i32));
    assert!(with_stage(TerminalPipelineStage::WorkerCoordLink as i32));
    assert!(!with_stage(
        TerminalPipelineStage::CoordWorkerIngress as i32
    ));
    assert!(!with_stage(99));

    let with_reason = |reason: i32| {
        let mut reply = well_shaped();
        reply.sessions[0].stages[0].reason = reason.into();
        is_terminal_pipeline_snapshot_wire_shape(&reply)
    };
    assert!(!with_reason(TerminalPipelineReason::Unspecified as i32));
    assert!(with_reason(TerminalPipelineReason::None as i32));
    assert!(with_reason(
        TerminalPipelineReason::RawMetadataPending as i32
    ));
    assert!(!with_reason(
        TerminalPipelineReason::RawMetadataPending as i32 + 1
    ));
}

#[test]
fn an_encoding_past_64_kib_is_refused() {
    let sessions: Vec<_> = (0..8)
        .map(|index| target(&format!("session-{index}"), ""))
        .collect();
    let mut reply = pipeline_reply("request-1", &sessions);
    for session in &mut reply.sessions {
        let mut stage = worker_stream_stage();
        stage.stream_id = "x".repeat(512);
        session.stages = vec![stage; 16];
    }
    assert!(!is_terminal_pipeline_snapshot_wire_shape(&reply));
    reply.sessions.truncate(7);
    assert!(is_terminal_pipeline_snapshot_wire_shape(&reply));
}

#[test]
fn the_projection_keeps_allowed_sessions_and_spells_counters_as_strings() {
    let mut reply = pipeline_reply(
        "request-1",
        &[target("mine", "view-1"), target("theirs", "")],
    );
    reply.sessions[0].stages[0].queue_bytes = u64::MAX;
    reply.sessions[0].stages[0].histogram_buckets = vec![3, u64::MAX];
    let allowed = HashSet::from(["mine".to_owned()]);

    let projected =
        serde_json::to_value(terminal_pipeline_diagnostic_snapshot(&reply, &allowed)).unwrap();

    assert_eq!(
        projected,
        serde_json::json!({
            "request_id": "request-1",
            "dropped_targets": 0,
            "dropped_records": 0,
            "sessions": [{
                "session_id": "mine",
                "view_id": "view-1",
                "stages": [{
                    "stage": TerminalPipelineStage::WorkerStream as i32,
                    "reason": TerminalPipelineReason::None as i32,
                    "generation": "7",
                    "stream_id": "stream-7",
                    "sequence": "11",
                    "queue_frames": "0",
                    "queue_bytes": "18446744073709551615",
                    "native_buffered_bytes": "0",
                    "oldest_age_ms": "0",
                    "count": "0",
                    "histogram_buckets": ["3", "18446744073709551615"],
                }],
            }],
        })
    );
}
