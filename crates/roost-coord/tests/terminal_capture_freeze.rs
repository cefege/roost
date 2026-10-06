//! Freezing coordinator capture records: over-budget frames kept as metadata,
//! the oldest-first trim to the wire budget, the nested envelope shape, the
//! frozen wire JSON admitted through its section, and a maximally trimmed
//! payload that still fits.
//!
//! Ports the freeze cases of
//! `apps/coord/tests/terminal/capture/terminal-capture-recorder.test.ts`; its
//! record cases are `terminal_capture_recorder.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod terminal_capture_support;

use roost_coord::terminal_capture::freeze::FreezeContext;
use roost_coord::terminal_capture::recorder::CoordinatorRecorder;
use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;
use roost_protocol::terminal_capture::bundle::{TERMINAL_INCIDENT_SCHEMA, TerminalCaptureLayer};
use roost_protocol::terminal_capture::envelope::{EvidenceOwner, check_terminal_capture_envelope};
use serde_json::{Value, json};
use terminal_capture_support::{
    CAPTURE_1, RECORDING_A, SESSION_A as SESSION, STREAM, admitted, canonical_frame,
    without_layer_section,
};

const ONE_WATCHER: usize = 1;
const NO_WATCHERS: usize = 0;
const OWNER: EvidenceOwner<'static> = EvidenceOwner {
    capture_id: CAPTURE_1,
    recording_id: RECORDING_A,
    session_id: SESSION,
};

/// The frozen wire JSON and its parsed payload. The wire bound is UTF-8 bytes
/// of the WHOLE payload, one nesting level included.
fn frozen(recorder: &CoordinatorRecorder) -> (String, Value) {
    let context = FreezeContext {
        git_sha: "test",
        captured_at_ms: 1,
    };
    let evidence = recorder.freeze(SESSION, CAPTURE_1, RECORDING_A, context);
    assert!(evidence.available);
    assert!(evidence.json.len() <= TERMINAL_CAPTURE_LIMITS.coordinator_evidence_bytes);
    assert_eq!(evidence.bytes, evidence.json.len());
    let payload = serde_json::from_str(&evidence.json).unwrap();
    (evidence.json, payload)
}

fn armed() -> CoordinatorRecorder {
    let recorder = CoordinatorRecorder::default();
    recorder.arm(SESSION, RECORDING_A);
    recorder
}

fn omission(section: &Value, reason: &str) -> Option<Value> {
    section["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["reason"] == reason)
        .cloned()
}

#[test]
fn a_frame_over_the_wire_budget_is_retained_as_metadata_and_never_left_at_the_head() {
    let recorder = armed();
    // Wider than the whole coordinator evidence budget: keeping its rows could
    // never be shipped, so the record stays and its canonical does not.
    let wide = canonical_frame(1, 64, 64, Some(&"x".repeat(120)));
    recorder.record(SESSION, &wide, admitted(true, 1, 0), NO_WATCHERS, 1);
    assert!(recorder.records(SESSION)[0].canonical.is_none());

    recorder.record(
        SESSION,
        &canonical_frame(2, 2, 1, None),
        admitted(false, 2, 1),
        NO_WATCHERS,
        1,
    );
    let records = recorder.records(SESSION);
    assert_eq!(records.len(), 1);
    assert_eq!(
        (records[0].stream.seq.as_str(), records[0].admitted_full),
        ("2", false)
    );
    assert!(records[0].canonical.is_some());

    let (_, payload) = frozen(&recorder);
    let section = &payload["coordinator"];
    let dropped = &section["dropped"];
    assert_eq!(
        [&dropped["rows"], &dropped["raw_bytes"], &dropped["samples"]],
        [&json!(64), &json!(0), &json!(0)]
    );
    assert_eq!(
        omission(section, "frame_over_budget").unwrap()["dropped_count"],
        1
    );
}

#[test]
fn freezing_trims_oldest_whole_records_to_the_budget_and_names_the_omission() {
    let recorder = armed();
    let text = "y".repeat(80);
    for seq in 1..=32 {
        let frame = canonical_frame(seq, 24, 12, Some(&text));
        recorder.record(
            SESSION,
            &frame,
            admitted(seq == 1, seq, seq - 1),
            ONE_WATCHER,
            1,
        );
    }
    let (_, payload) = frozen(&recorder);
    let section = &payload["coordinator"];
    let records = section["records"].as_array().unwrap();
    assert!(!records.is_empty() && records.len() < 32);
    // The newest evidence is what the incident needs, and the head still
    // carries a canonical a replay can start from.
    assert_eq!(records.last().unwrap()["stream"]["seq"], "32");
    assert!(!records[0]["canonical"].is_null());
    let trimmed = omission(section, "evidence_trimmed").unwrap();
    assert_eq!(trimmed["dropped_count"], 32 - records.len() as u64);
}

#[test]
fn the_payload_nests_its_section_under_the_layer_and_keeps_identity_on_the_envelope() {
    let recorder = armed();
    recorder.record(
        SESSION,
        &canonical_frame(7, 2, 1, None),
        admitted(true, 7, 0),
        ONE_WATCHER,
        1,
    );
    let (_, payload) = frozen(&recorder);

    // Envelope: capture identity, and only that.
    let mut keys: Vec<&str> = payload
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        [
            "capture_id",
            "coordinator",
            "layer",
            "recording_id",
            "schema",
            "session_id"
        ]
    );
    assert_eq!(payload["schema"], TERMINAL_INCIDENT_SCHEMA);
    assert_eq!(
        [
            &payload["layer"],
            &payload["capture_id"],
            &payload["recording_id"],
            &payload["session_id"]
        ],
        [
            &json!("coordinator"),
            &json!(CAPTURE_1),
            &json!(RECORDING_A),
            &json!(SESSION)
        ]
    );
    // Section: the layer's own evidence, NOT the envelope's identity. A
    // flattened payload passes an identity check and then fails as a section.
    let section = &payload["coordinator"];
    assert_eq!(
        (&section["layer"], &section["valid"]),
        (&json!("coordinator"), &json!(true))
    );
    assert_eq!(section["geometry"], json!({ "cols": 80, "rows": 2 }));
    assert_eq!(
        (
            &section["snapshot"]["stream_id"],
            &section["snapshot"]["seq"]
        ),
        (&json!(STREAM), &json!("7"))
    );
    assert_eq!(section["records"].as_array().unwrap().len(), 1);
    assert!(section["captured_at_ms"].is_u64());
    let process = &section["process"];
    assert_eq!(process["layer"], "coordinator");
    for absent in ["wasm_identity", "worker_fp", "viewer_id", "user_agent"] {
        assert!(process[absent].is_null(), "{absent}");
    }
    assert!(process["process_id"].is_string());
    assert_eq!(section["omissions"], json!([]));
    assert!(payload.get("records").is_none() && payload.get("captured_at_ms").is_none());
}

#[test]
fn the_frozen_wire_json_itself_is_admitted_through_its_nested_section() {
    let recorder = armed();
    recorder.record(
        SESSION,
        &canonical_frame(4, 2, 1, None),
        admitted(true, 4, 0),
        ONE_WATCHER,
        1,
    );
    // The exact string the bridge forwards, never a re-serialized fixture: a
    // hand-built payload proves the checker, not the producer.
    let (json, payload) = frozen(&recorder);

    let checked = check_terminal_capture_envelope(&json, TerminalCaptureLayer::Coordinator, &OWNER)
        .expect("the frozen coordinator payload is admitted");
    // Reached through the `coordinator` member.
    assert_eq!(checked.section["records"].as_array().unwrap().len(), 1);
    assert!(checked.section["captured_at_ms"].is_u64());
    // The coordinator never authors a trigger; the triggering layer ships it.
    assert_eq!(checked.trigger, None);

    // The production defect: the capture identity still matches, so only the
    // section check can catch it.
    let flattened = without_layer_section(payload).to_string();
    let refusal =
        check_terminal_capture_envelope(&flattened, TerminalCaptureLayer::Coordinator, &OWNER)
            .expect_err("a payload without its section is refused");
    assert_eq!(
        format!("{:?}:{}", refusal.code, refusal.field),
        "EvidenceMalformed:coordinator"
    );
}

#[test]
fn a_maximally_trimmed_payload_still_fits_the_coordinator_evidence_budget() {
    let recorder = armed();
    // Every omission kind at once -- eviction, over-budget rows and the freeze
    // trim -- so the header is at its largest while records fill the rest.
    let wide = canonical_frame(1, 64, 64, Some(&"z".repeat(120)));
    recorder.record(SESSION, &wide, admitted(true, 1, 0), ONE_WATCHER, 1);
    let total = TERMINAL_CAPTURE_LIMITS.layer_entries as u64 + 4;
    let text = "w".repeat(96);
    for seq in 2..=total {
        let frame = canonical_frame(seq, 24, 16, Some(&text));
        recorder.record(
            SESSION,
            &frame,
            admitted(false, seq, seq - 1),
            ONE_WATCHER,
            1,
        );
    }
    let (_, payload) = frozen(&recorder);
    let section = &payload["coordinator"];
    // The reserve must leave room for the envelope AND the header it names, so
    // a trimmed capture still ships evidence instead of reporting unavailable.
    let records = section["records"].as_array().unwrap();
    assert!(!records.is_empty());
    assert!(!records[0]["canonical"].is_null());
    let mut reasons: Vec<&str> = section["omissions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["reason"].as_str().unwrap())
        .collect();
    reasons.sort_unstable();
    assert_eq!(
        reasons,
        ["evidence_trimmed", "frame_over_budget", "segment_evicted"]
    );
    assert!(section["dropped"]["records"].as_u64().unwrap() > 0);
}
