//! Evidence payloads between the three capture layers: an envelope PLUS the
//! layer's section nested under a member named for that layer. A flattened
//! payload once validated as an envelope, failed as a section, and silently
//! cost every capture its browser evidence. Ports `packages/protocol/tests/
//! terminal-capture-envelope.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::terminal_capture::TerminalCaptureErrorCode as Code;
use roost_protocol::terminal_capture::bundle::TerminalCaptureLayer as Layer;
use roost_protocol::terminal_capture::envelope::{EvidenceOwner, check_terminal_capture_envelope};
use roost_protocol::terminal_capture::validate::validate_terminal_incident_bundle;
use serde_json::{Value, json};

const SESSION: &str = "22222222-2222-4222-8222-222222222222";
const CAPTURE: &str = "33333333-3333-4333-8333-333333333333";
const RECORDING: &str = "44444444-4444-4444-8444-444444444444";

const OWNER: EvidenceOwner<'static> = EvidenceOwner {
    capture_id: CAPTURE,
    recording_id: RECORDING,
    session_id: SESSION,
};

fn trigger() -> Value {
    json!({
        "reason": "history_identity", "origin": "browser", "at_ms": 1_700_000_000_000_u64,
        "stream_id": "11111111-1111-4111-8111-111111111111", "grid_epoch": "epoch:1", "seq": "9",
        "detail": "history_duplicate_index", "occurrence_count": 3,
    })
}

fn header(layer: &str) -> serde_json::Map<String, Value> {
    let value = json!({
        "layer": layer,
        "captured_at_ms": 1_700_000_000_001_u64,
        "process": {
            "layer": layer, "process_id": format!("{layer}-1"), "git_sha": "abc1234", "artifact_version": "2.0.0",
            "wasm_identity": null, "worker_fp": null, "viewer_id": null, "user_agent": null,
        },
        "stream": null,
        "geometry": null,
        "dropped": { "records": 0, "bytes": 0, "rows": 0, "raw_bytes": 0, "samples": 0 },
        "omissions": [],
    });
    value.as_object().cloned().unwrap()
}

fn browser_section() -> Value {
    let mut section = header("browser");
    for (key, value) in [
        ("events", json!([])),
        ("replica", Value::Null),
        ("trigger_state", Value::Null),
        ("pre_repair_state", Value::Null),
        ("post_repair_state", Value::Null),
        ("current_state", Value::Null),
    ] {
        section.insert(key.to_owned(), value);
    }
    Value::Object(section)
}

fn coordinator_section() -> Value {
    let mut section = header("coordinator");
    section.insert("records".to_owned(), json!([]));
    section.insert("snapshot".to_owned(), Value::Null);
    section.insert("valid".to_owned(), json!(true));
    Value::Object(section)
}

fn envelope(layer: &str) -> Value {
    json!({ "schema": "roost.terminal-incident.v1", "layer": layer, "capture_id": CAPTURE, "recording_id": RECORDING, "session_id": SESSION })
}

fn browser_payload() -> Value {
    let mut payload = envelope("browser");
    payload["trigger"] = trigger();
    payload["browser"] = browser_section();
    payload
}

fn coordinator_payload() -> Value {
    let mut payload = envelope("coordinator");
    payload["coordinator"] = coordinator_section();
    payload
}

fn check(
    payload: &Value,
    layer: Layer,
) -> Result<roost_protocol::terminal_capture::envelope::CheckedEvidence, (Code, String)> {
    check_terminal_capture_envelope(&payload.to_string(), layer, &OWNER)
        .map_err(|refusal| (refusal.code, refusal.field))
}

fn bundle(browser: Value, coordinator: Value) -> Value {
    json!({
        "schema": "roost.terminal-incident.v1", "capture_id": CAPTURE, "recording_id": RECORDING, "session_id": SESSION,
        "written_at_ms": 1_700_000_000_002_u64, "trigger": trigger(),
        "coverage": {
            "cell_replay": "complete", "cell_replay_reasons": ["complete"],
            "core_replay": "complete", "core_replay_reasons": ["complete"],
            "core_comparison": "complete", "core_comparison_reasons": ["complete"],
        },
        "browser": browser, "coordinator": coordinator, "worker": null,
    })
}

#[test]
fn a_nested_browser_payload_yields_its_section_and_its_trigger() {
    let checked = check(&browser_payload(), Layer::Browser).unwrap();
    assert_eq!(
        checked.section["captured_at_ms"],
        json!(1_700_000_000_001_u64)
    );
    assert_eq!(checked.section["layer"], json!("browser"));
    // The browser authors the trigger: only it knows which invariant fired.
    let trigger = checked.trigger.expect("the browser ships its trigger");
    assert_eq!(
        (
            trigger["detail"].clone(),
            trigger["occurrence_count"].clone()
        ),
        (json!("history_duplicate_index"), json!(3))
    );
}

#[test]
fn a_nested_coordinator_payload_yields_its_section() {
    let checked = check(&coordinator_payload(), Layer::Coordinator).unwrap();
    assert_eq!(checked.section["records"], json!([]));
    assert!(checked.trigger.is_none());
}

#[test]
fn a_flattened_payload_is_refused_at_the_envelope_naming_the_layer() {
    let mut flattened = browser_payload().as_object().cloned().unwrap();
    flattened.remove("browser");
    for (key, value) in browser_section().as_object().unwrap() {
        flattened.insert(key.clone(), value.clone());
    }
    assert_eq!(
        check(&Value::Object(flattened), Layer::Browser).unwrap_err(),
        (Code::EvidenceMalformed, "browser".to_owned())
    );
}

#[test]
fn a_payload_whose_section_member_is_not_an_object_is_refused() {
    for section in [Value::Null, json!("browser"), json!(7), json!([])] {
        let mut payload = browser_payload();
        payload["browser"] = section;
        assert_eq!(check(&payload, Layer::Browser).unwrap_err().1, "browser");
    }
}

#[test]
fn cross_session_and_cross_capture_evidence_is_refused_before_any_forward() {
    for (field, value) in [
        ("capture_id", RECORDING),
        ("recording_id", CAPTURE),
        ("session_id", CAPTURE),
    ] {
        let mut payload = browser_payload();
        payload[field] = json!(value);
        assert_eq!(
            check(&payload, Layer::Browser).unwrap_err(),
            (Code::PermissionDenied, field.to_owned())
        );
    }
}

#[test]
fn an_envelope_placed_where_a_section_belongs_fails_the_bundle_gate() {
    let refusal =
        validate_terminal_incident_bundle(&bundle(browser_payload(), Value::Null)).unwrap_err();
    assert_eq!(refusal.field, "browser.captured_at_ms");
}

#[test]
fn the_properly_unwrapped_sections_pass_the_bundle_gate() {
    assert_eq!(
        validate_terminal_incident_bundle(&bundle(browser_section(), coordinator_section())),
        Ok(())
    );
}
