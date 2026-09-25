//! The event union's own decode contract: the JSON tag, the timestamp bound,
//! the session rows a snapshot carries, and the serialized-envelope bound that
//! only an `agent_reference` event is held to.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use roost_protocol::wire::event::SessionEvent;

const FINGERPRINT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SESSION: &str = "00000000-0000-4000-8000-000000000001";

fn row() -> Value {
    json!({
        "id": SESSION,
        "worker_fp": FINGERPRINT,
        "channel": 1,
        "kind": "shell",
        "cwd": "/repo",
        "workspace_id": null,
        "status": "open",
        "created_at": 1,
        "closed_at": null,
        "custom_title": null,
    })
}

/// Parse one event, naming the failing variant in the panic so a regression
/// points at the variant rather than at the file.
fn event(value: Value) -> SessionEvent {
    SessionEvent::parse(value).expect("the event satisfies the contract")
}

#[test]
fn an_event_round_trips_through_its_json_tag() {
    let event = event(json!({
        "kind": "opened", "session_id": SESSION, "worker_fp": FINGERPRINT, "channel": 1,
        "session_kind": "shell", "cwd": "/repo", "ts": 1,
    }));
    let encoded = serde_json::to_value(&event).expect("serializable");
    assert_eq!(encoded["kind"], json!("opened"));
    assert_eq!(encoded["trace_id"], Value::Null);
    assert_eq!(SessionEvent::parse(encoded).unwrap(), event);
}

#[test]
fn a_non_positive_timestamp_is_refused() {
    let mut value = json!({
        "kind": "opened", "session_id": SESSION, "worker_fp": FINGERPRINT, "channel": 1,
        "session_kind": "shell", "cwd": "/repo", "ts": 1,
    });
    value["ts"] = json!(0);
    assert_eq!(
        SessionEvent::parse(value).unwrap_err().field,
        "session_event.ts"
    );
}

#[test]
fn a_snapshot_carries_each_row_through_the_session_contract() {
    let mut broken = row();
    broken["created_at"] = json!(0);
    let refused = SessionEvent::parse(json!({
        "kind": "snapshot", "worker_fp": FINGERPRINT, "ts": 2, "sessions": [broken],
    }));
    assert_eq!(
        refused.unwrap_err().field,
        "session_event.sessions[0].created_at"
    );
}

#[test]
fn a_reference_at_its_per_kind_bound_is_admitted_and_the_envelope_bound_is_what_refuses() {
    // 4096 bytes is exactly the path bound, so this event is inside BOTH limits:
    // the per-kind value cap and the 8192-byte serialized envelope. A fixture
    // that sits between them proves neither, because a single reference cannot
    // exceed the envelope while staying inside its own cap.
    let at_the_bound = SessionEvent::parse(json!({
        "kind": "agent_reference",
        "session_id": SESSION,
        "reference": {
            "schema_version": 1,
            "agent_id": "omp",
            "kind": "path",
            "value": format!("/{}", "a".repeat(4_095)),
        },
        "ts": 1,
    }));
    assert!(at_the_bound.is_ok(), "{at_the_bound:?}");

    let over_the_kind_bound = SessionEvent::parse(json!({
        "kind": "agent_reference",
        "session_id": SESSION,
        "reference": {
            "schema_version": 1,
            "agent_id": "omp",
            "kind": "path",
            "value": format!("/{}", "a".repeat(4_096)),
        },
        "ts": 1,
    }));
    assert_eq!(
        over_the_kind_bound.unwrap_err().field,
        "session_event.reference.value"
    );

    // The envelope bound is the one a `trace_id` can reach, because the trace
    // id is the only unbounded part of this variant.
    let oversized_envelope = SessionEvent::parse(json!({
        "kind": "agent_reference",
        "session_id": SESSION,
        "reference": {
            "schema_version": 1,
            "agent_id": "omp",
            "kind": "id",
            "value": "conversation-1",
        },
        "trace_id": "a".repeat(9_000),
        "ts": 1,
    }));
    assert!(
        oversized_envelope.is_err(),
        "a nine-thousand-character trace id must not ride the log"
    );
}
