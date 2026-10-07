//! The event union's own decode contract: the JSON tag, the timestamp bound,
//! and the session rows a snapshot carries.
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
