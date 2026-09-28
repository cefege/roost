//! The layered terminal probe the coordinator's DiagSnapshot slice consumes:
//! the browser half's view status, the join that normalizes the coordinator's
//! answer, and the mount-scoped geometry-proof ledger the paint proofs record
//! into.
//!
//! Ports v2 `apps/web/src/renderer/terminalDiagSnapshot.ts` (`recordTerminalGeometryProof`,
//! the view handle status) and `apps/web/src/smoke/smokeTerminalStreamProbe.ts`
//! (`normalizeTerminalStreamProbe`: a missing or malformed layer stays explicit
//! instead of reading as healthy).
#![cfg(feature = "smoke")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::terminal::view::{TerminalView, ViewAnswer};
use roost_web::smoke::browser_snapshot::GeometryProofs;
use roost_web::smoke::stream_diagnostics::view_status;
use roost_web::smoke::stream_probe::{normalize_terminal_stream_probe, parse_coordinator_snapshot};
use serde_json::{Value, json};

const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
const OTHER: &str = "00000000-0000-4000-8000-00000000000b";
const FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn opened_view() -> TerminalView {
    TerminalView::opened("00000000-0000-4000-8000-0000000000c1", 80, 24, 0)
}

fn settled(view: &mut TerminalView, accepted: bool) {
    view.mark_published(1, 10);
    view.acknowledge(1, 20);
    view.answer = Some(ViewAnswer {
        revision: view.revision,
        accepted,
    });
}

fn probe(raw: &Value) -> Value {
    let browser = json!({ "captured_at_ms": 1, "session_id": SESSION });
    normalize_terminal_stream_probe(SESSION, browser, raw).expect("the layers join")
}

fn coord_snapshot() -> Value {
    json!({
        "captured_at_ms": 7,
        "coord": {
            "build": { "git_sha": "abc123", "artifact_version": "3.0.0" },
            "sessions": {
                SESSION: {
                    "route": { "worker_fp": FP },
                    "terminal_view": {
                        "activeViews": 2,
                        "parkedViews": 1,
                        "streamId": "stream-1",
                        "unavailable": false,
                        "effective": { "cols": 100, "rows": 40 },
                    },
                    "viewers": [{ "view_id": "a" }, { "view_id": "b" }],
                },
            },
        },
        "workers": {
            FP: {
                "status": "ok",
                "response_ms": 12.5,
                "snapshot": {
                    "build": { "git_sha": "def456" },
                    "sessions": { SESSION: { "pty_pid": 4321 } },
                },
            },
        },
    })
}

#[test]
fn a_view_the_authority_answered_at_this_revision_reads_as_its_verdict() {
    let mut view = opened_view();
    settled(&mut view, true);
    assert_eq!(view_status(&view), Some("accepted"));
    settled(&mut view, false);
    assert_eq!(view_status(&view), Some("rejected"));
}

#[test]
fn a_view_answered_before_its_latest_intent_reads_as_pending() {
    let mut view = opened_view();
    settled(&mut view, true);
    assert_eq!(view_status(&view), Some("accepted"));

    view.resize(100, 30);
    assert_eq!(
        view_status(&view),
        Some("pending"),
        "an answer to an older revision cannot settle a newer intent"
    );
}

#[test]
fn a_view_that_has_published_but_never_answered_reads_as_pending() {
    let mut view = opened_view();
    assert_eq!(view_status(&view), None, "nothing is out yet");
    view.mark_published(1, 10);
    assert_eq!(view_status(&view), Some("pending"));
}

#[test]
fn a_joined_probe_carries_all_three_layers_for_the_named_session() {
    let joined = probe(&coord_snapshot());
    assert_eq!(joined["captured_at_ms"], json!(7));
    assert_eq!(joined["coord"]["build"]["git_sha"], json!("abc123"));
    let control = &joined["coord"]["terminal_control"];
    assert_eq!(control["active_view_count"], json!(2));
    assert_eq!(control["parked_view_count"], json!(1));
    assert_eq!(control["stream_id"], json!("stream-1"));
    assert_eq!(control["effective_cols"], json!(100));
    assert_eq!(control["effective_rows"], json!(40));
    assert_eq!(control["viewer_inputs"].as_array().unwrap().len(), 2);
    assert_eq!(joined["worker"]["worker_fp"], json!(FP));
    assert_eq!(joined["worker"]["status"], json!("ok"));
    assert_eq!(joined["worker"]["response_ms"], json!(12.5));
    assert_eq!(joined["worker"]["session"]["pty_pid"], json!(4321));
    assert_eq!(joined["worker"]["error"], Value::Null);
}

#[test]
fn a_session_the_coordinator_does_not_hold_reads_as_null_not_as_another_session() {
    let joined = probe(&coord_snapshot());
    let foreign = normalize_terminal_stream_probe(
        OTHER,
        json!({ "captured_at_ms": 1, "session_id": OTHER }),
        &coord_snapshot(),
    )
    .expect("the layers join");
    assert_eq!(
        foreign["coord"]["terminal_control"],
        Value::Null,
        "another session's terminal_view must never be read as this one's"
    );
    assert_eq!(foreign["worker"]["worker_fp"], Value::Null);
    assert_eq!(foreign["worker"]["status"], json!("missing"));
    assert_eq!(foreign["worker"]["session"], Value::Null);
    assert_eq!(joined["worker"]["status"], json!("ok"));
}

#[test]
fn an_absent_or_malformed_coordinator_layer_stays_explicit() {
    assert_eq!(
        normalize_terminal_stream_probe(SESSION, json!({}), &json!([])).unwrap_err(),
        "coordinator diagnostic snapshot was not an object"
    );
    let refusal = parse_coordinator_snapshot("not json").unwrap_err();
    assert!(
        refusal.starts_with("coordinator diagnostic snapshot was invalid JSON: "),
        "an unparseable layer is refused, not read as an empty one: {refusal}"
    );
    let joined = probe(&json!({}));
    assert_eq!(joined["coord"], Value::Null);
    assert_eq!(joined["worker"]["status"], json!("missing"));
    assert_eq!(joined["worker"]["build"], Value::Null);
    assert_eq!(joined["worker"]["response_ms"], Value::Null);
    assert_eq!(
        joined["captured_at_ms"],
        json!(1),
        "with no coordinator clock, the browser's own is the capture time"
    );
}

#[test]
fn a_worker_error_envelope_carries_its_code_and_message_and_no_snapshot() {
    let raw = json!({
        "coord": { "sessions": { SESSION: { "route": { "worker_fp": FP } } } },
        "workers": { FP: { "status": "error", "error": { "code": "timeout", "message": "no reply" } } },
    });
    let joined = probe(&raw);
    assert_eq!(joined["worker"]["status"], json!("error"));
    assert_eq!(joined["worker"]["error"]["code"], json!("timeout"));
    assert_eq!(joined["worker"]["error"]["message"], json!("no reply"));
    assert_eq!(
        joined["worker"]["session"],
        Value::Null,
        "an errored worker has no session layer to read"
    );
}

#[test]
fn a_non_string_error_member_reads_as_null_rather_than_as_the_wrong_type() {
    let raw = json!({
        "coord": { "sessions": { SESSION: { "route": { "worker_fp": FP } } } },
        "workers": { FP: { "status": "error", "error": { "code": 7, "message": ["no"] } } },
    });
    let joined = probe(&raw);
    assert_eq!(
        joined["worker"]["status"],
        json!("error"),
        "the envelope must reach the error arm, or the assertions below read null for free"
    );
    assert_eq!(joined["worker"]["error"]["code"], Value::Null);
    assert_eq!(joined["worker"]["error"]["message"], Value::Null);
}

#[test]
fn a_coord_build_that_is_not_a_record_reads_as_two_nulls() {
    let raw = json!({ "coord": { "build": 7, "sessions": {} } });
    let joined = probe(&raw);
    assert_eq!(joined["coord"]["build"]["git_sha"], Value::Null);
    assert_eq!(joined["coord"]["build"]["artifact_version"], Value::Null);
}

#[test]
fn a_geometry_proof_belongs_to_the_mount_that_proved_it() {
    let mut proofs = GeometryProofs::default();
    let proof = json!({ "sessionId": SESSION, "marker": "MARK" });
    proofs.record(SESSION, Some(1), &proof);
    assert_eq!(proofs.latest(SESSION, Some(1)), Some(proof.clone()));

    assert_eq!(
        proofs.latest(SESSION, Some(2)),
        None,
        "a remount has proved nothing, and must not inherit the old mount's proof"
    );
    assert_eq!(proofs.latest(OTHER, Some(1)), None);
}

#[test]
fn a_proof_naming_another_session_or_no_mount_is_not_kept() {
    let mut proofs = GeometryProofs::default();
    proofs.record(SESSION, None, &json!({ "sessionId": SESSION }));
    proofs.record(SESSION, Some(1), &json!({ "sessionId": OTHER }));
    proofs.record(SESSION, Some(1), &json!({ "marker": "MARK" }));
    assert_eq!(proofs.latest(SESSION, Some(1)), None);
}
