//! The incident-bundle validator is the gate between a corrupt or truncated
//! capture and an attribution claim, so these pin what it must REFUSE: a bundle
//! that would let a replay fold onto a state nobody shipped, or read an offset
//! that rounded — and every refusal names a field, never the value. Ports
//! `packages/protocol/tests/terminal-capture-validate.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::TerminalCaptureErrorCode as Code;
use roost_protocol::terminal_capture::validate::validate_terminal_incident_bundle;
use roost_protocol::terminal_capture::validate_fields::is_decimal_uint64;
use serde_json::{Value, json};

const SESSION: &str = "22222222-2222-4222-8222-222222222222";
const CAPTURE: &str = "33333333-3333-4333-8333-333333333333";
const RECORDING: &str = "44444444-4444-4444-8444-444444444444";
const STREAM: &str = "11111111-1111-4111-8111-111111111111";

fn stream_identity(seq: &str) -> Value {
    json!({ "stream_id": STREAM, "grid_epoch": "epoch:1", "seq": seq, "base_seq": null, "cols": 4, "rows": 1 })
}

fn full_frame(seq: u64) -> Value {
    json!({
        "streamId": STREAM, "gridEpoch": "epoch:1", "cols": 4, "rows": 1,
        "cursorRow": 0, "cursorCol": 0, "cursorVisible": true, "altScreen": false,
        "cursorKeysApp": false, "bracketedPaste": false, "mouseTracking": 0,
        "mouseSgr": false, "focusEvents": false, "full": true,
        "viewportRows": [{ "index": 0, "spans": [{ "text": "ok", "fg": 256, "bg": 256, "flags": 0, "columns": 2 }] }],
        "scrollbackRows": [], "scrollbackAppend": [], "scrollbackTotal": 0, "sbBase": 0, "baseSeq": 0, "seq": seq,
    })
}

fn emission(segment: &str, seq: u64, frame: Value) -> Value {
    json!({
        "segment_id": segment, "emitted_at_ms": 1_700_000_000_001_u64, "stream": stream_identity(&seq.to_string()),
        "full": true, "frame": frame, "comparison": "equal", "difference": null,
    })
}

fn raw(start: Value, end: Value) -> Value {
    json!({ "segment_id": "seg-1", "at_ms": 1, "start_offset": start, "end_offset": end, "base64": "" })
}

fn worker_section() -> Value {
    json!({
        "layer": "worker",
        "captured_at_ms": 1_700_000_000_000_u64,
        "process": {
            "layer": "worker", "process_id": "worker-1", "git_sha": "abc1234", "artifact_version": "2.0.0",
            "wasm_identity": "sha256:dead", "worker_fp": "fp1", "viewer_id": null, "user_agent": null,
        },
        "stream": stream_identity("7"),
        "geometry": { "cols": 4, "rows": 1 },
        "dropped": { "records": 0, "bytes": 0, "rows": 0, "raw_bytes": 0, "samples": 0 },
        "omissions": [],
        "segments": [{
            "segment_id": "seg-1", "stream_id": STREAM, "grid_epoch": "epoch:1", "core_incarnation": 0,
            "opened_at_ms": 1_700_000_000_000_u64, "closed_at_ms": null, "open_reason": "armed",
            "geometry": { "cols": 4, "rows": 1 }, "open_offset": "0",
        }],
        "emissions": [emission("seg-1", 1, full_frame(1))],
        "core_samples": [],
        "sampling": {
            "sampled": 0, "skipped_interval": 0, "skipped_budget": 0, "skipped_grid": 0,
            "suppressed_until_ms": null, "max_elapsed_us": 0,
        },
        "resizes": [],
        "raw": [{ "segment_id": "seg-1", "at_ms": 1_700_000_000_001_u64, "start_offset": "0", "end_offset": "2", "base64": "b2s=" }],
        "byte_capture": null,
        "core_scrollback_tail": [],
        "history_rows": [],
        "history_ranges": [],
        "scrollback_total": 0,
        "scrollback_origin": "0",
    })
}

fn bundle_with(worker: Value) -> Value {
    json!({
        "schema": "roost.terminal-incident.v1",
        "capture_id": CAPTURE,
        "recording_id": RECORDING,
        "session_id": SESSION,
        "written_at_ms": 1_700_000_000_002_u64,
        "trigger": {
            "reason": "manual", "origin": "worker", "at_ms": 1_700_000_000_002_u64, "stream_id": STREAM,
            "grid_epoch": "epoch:1", "seq": "1", "detail": null, "occurrence_count": 1,
        },
        "coverage": {
            "cell_replay": "complete", "cell_replay_reasons": ["complete"],
            "core_replay": "partial", "core_replay_reasons": ["missing_initial_prefix"],
            "core_comparison": "unavailable", "core_comparison_reasons": ["layer_unavailable"],
        },
        "browser": null,
        "coordinator": null,
        "worker": worker,
    })
}

fn refused(bundle: &Value) -> (Code, String) {
    let refusal =
        validate_terminal_incident_bundle(bundle).expect_err("the gate refuses this bundle");
    (refusal.code, refusal.field)
}

#[test]
fn a_minimal_worker_only_bundle_is_accepted() {
    assert_eq!(
        validate_terminal_incident_bundle(&bundle_with(worker_section())),
        Ok(())
    );
}

#[test]
fn a_foreign_schema_literal_is_refused() {
    let mut bundle = bundle_with(worker_section());
    bundle["schema"] = json!("other.v1");
    assert_eq!(
        refused(&bundle),
        (Code::EvidenceMalformed, "schema".to_owned())
    );
}

#[test]
fn a_non_uuid_identifier_is_refused() {
    let mut bundle = bundle_with(worker_section());
    bundle["capture_id"] = json!("not-a-uuid");
    assert_eq!(
        refused(&bundle),
        (Code::InvalidArgument, "capture_id".to_owned())
    );
}

#[test]
fn every_coverage_axis_needs_a_reason() {
    let mut bundle = bundle_with(worker_section());
    bundle["coverage"]["core_replay_reasons"] = json!([]);
    assert_eq!(refused(&bundle).1, "coverage.core_replay_reasons");
}

#[test]
fn a_per_segment_sequence_that_moves_backwards_is_refused() {
    let mut worker = worker_section();
    worker["emissions"] = json!([
        emission("seg-1", 5, full_frame(5)),
        emission("seg-1", 4, full_frame(4))
    ]);
    assert_eq!(
        refused(&bundle_with(worker)),
        (
            Code::InvalidArgument,
            "worker.emissions[1].stream.seq".to_owned()
        )
    );
}

#[test]
fn an_emission_referencing_an_unknown_segment_is_refused() {
    let mut worker = worker_section();
    worker["emissions"] = json!([emission("seg-missing", 1, full_frame(1))]);
    assert_eq!(
        refused(&bundle_with(worker)).1,
        "worker.emissions[0].segment_id"
    );
}

#[test]
fn a_full_frame_whose_viewport_is_not_dense_is_refused() {
    let mut frame = full_frame(1);
    frame["rows"] = json!(2);
    let mut worker = worker_section();
    worker["emissions"] = json!([emission("seg-1", 1, frame)]);
    assert_eq!(
        refused(&bundle_with(worker)).1,
        "worker.emissions[0].frame.viewportRows"
    );
}

#[test]
fn raw_offsets_must_be_decimal_uint64_and_run_forwards() {
    // A bad single offset names that offset; only the cross-field ordering
    // names the pair, because no one field is the wrong one.
    for (start, end, field) in [
        (json!(0), json!("2"), "worker.raw[0].start_offset"),
        (json!("007"), json!("2"), "worker.raw[0].start_offset"),
        (json!("9"), json!("2"), "worker.raw[0].offsets"),
    ] {
        let mut worker = worker_section();
        worker["raw"] = json!([raw(start, end)]);
        assert_eq!(
            refused(&bundle_with(worker)),
            (Code::InvalidArgument, field.to_owned())
        );
    }
}

#[test]
fn a_record_array_past_the_entry_bound_is_refused() {
    let mut worker = worker_section();
    worker["raw"] = Value::Array(
        (0..=TERMINAL_CAPTURE_LIMITS.layer_entries)
            .map(|_| raw(json!("0"), json!("0")))
            .collect(),
    );
    assert_eq!(
        refused(&bundle_with(worker)),
        (Code::ResourceExhausted, "worker.raw".to_owned())
    );
}

#[test]
fn a_refusal_carries_no_terminal_text() {
    let mut frame = full_frame(1);
    frame["viewportRows"] = json!([{ "index": 0, "spans": [{ "text": "sk-secret-value", "fg": 256, "bg": 256, "flags": 0, "columns": 0 }] }]);
    let mut worker = worker_section();
    worker["emissions"] = json!([emission("seg-1", 1, frame)]);
    let refusal = validate_terminal_incident_bundle(&bundle_with(worker))
        .expect_err("a zero-column span is refused");
    assert!(!format!("{refusal:?}").contains("secret"), "{refusal:?}");
}

#[test]
fn decimal_uint64_accepts_exact_offsets_and_refuses_lossy_or_padded_forms() {
    assert!(is_decimal_uint64(Some(&json!("0"))));
    assert!(is_decimal_uint64(Some(&json!("18446744073709551615"))));
    for refused in [
        json!("18446744073709551616"),
        json!("007"),
        json!("-1"),
        json!("1e3"),
        json!(7),
    ] {
        assert!(!is_decimal_uint64(Some(&refused)), "{refused}");
    }
}
