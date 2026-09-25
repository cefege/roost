//! The volatile agent-status contracts as the wire exercises them.
//!
//! Every case builds the JSON a worker or a browser actually sends, because
//! the rules this file holds are the ones a decode of that JSON has to apply.

use super::*;

const SESSION: &str = "11111111-1111-4111-8111-111111111111";
const EPOCH: &str = "22222222-2222-4222-8222-222222222222";
const OCCUPANT: &str = "33333333-3333-4333-8333-333333333333";

fn status_json() -> serde_json::Value {
    serde_json::json!({
        "session_id": SESSION,
        "agent_id": "omp",
        "state": "working",
        "revision": 4,
        "completed_revision": 2,
        "updated_at": 1234,
        "active": true,
    })
}

fn identified_json() -> serde_json::Value {
    let mut value = status_json();
    value["status_epoch"] = serde_json::json!(EPOCH);
    value["occupant_id"] = serde_json::json!(OCCUPANT);
    value["source"] = serde_json::json!("integration");
    value
}

#[test]
fn an_identityless_status_stays_valid() {
    let status = AgentStatus::parse(status_json()).unwrap();
    assert!(!status.common.occupant_exited);
    assert_eq!(status.common.message, None);
    assert!(!is_identified_agent_status(&status.common));
}

#[test]
fn a_retained_status_cannot_arrive_deactivated() {
    let mut value = status_json();
    value["active"] = serde_json::json!(false);
    assert_eq!(
        AgentStatus::parse(value.clone()).unwrap_err().field,
        "agent_status.active"
    );
    // The same payload is a valid deletion update, which is the other type.
    let update = AgentStatusUpdate::parse(value).unwrap();
    assert!(!update.active);
}

#[test]
fn an_agent_id_is_bounded_and_shape_checked() {
    let over_length = "a".repeat(AGENT_ID_MAX_LENGTH + 1);
    assert!(AgentId::try_from("codex").is_ok());
    assert!(AgentId::try_from("agent-2").is_ok());
    for rejected in ["", "Codex", "agent_name", over_length.as_str()] {
        assert!(
            AgentId::try_from(rejected).is_err(),
            "{rejected:?} accepted"
        );
    }
}

#[test]
fn every_partial_identity_triple_is_rejected() {
    let mut value = status_json();
    value["status_epoch"] = serde_json::json!(EPOCH);
    assert!(AgentStatus::parse(value.clone()).is_err());
    value.as_object_mut().unwrap().remove("status_epoch");
    value["occupant_id"] = serde_json::json!(OCCUPANT);
    assert!(AgentStatus::parse(value.clone()).is_err());
    value.as_object_mut().unwrap().remove("occupant_id");
    value["source"] = serde_json::json!("screen");
    assert!(AgentStatus::parse(value).is_err());
}

#[test]
fn only_a_complete_triple_narrows_to_an_identity() {
    let status = AgentStatus::parse(identified_json()).unwrap();
    let identity = agent_status_identity(&status.common).expect("complete triple");
    assert_eq!(identity.source, AgentStatusSource::Integration);
    assert_eq!(identity.status_epoch.as_str(), EPOCH);
    assert_eq!(identity.occupant_id.as_str(), OCCUPANT);
}

#[test]
fn a_completion_may_not_run_ahead_of_the_revision() {
    let mut value = status_json();
    value["completed_revision"] = serde_json::json!(5);
    assert_eq!(
        AgentStatus::parse(value).unwrap_err().field,
        "agent_status.completed_revision"
    );
}

#[test]
fn revisions_and_timestamps_must_stay_inside_the_safe_range() {
    for (field, patch) in [
        ("revision", serde_json::json!(MAX_SAFE_INTEGER + 1)),
        ("revision", serde_json::json!(-1)),
        ("updated_at", serde_json::json!(MAX_SAFE_INTEGER + 1)),
    ] {
        let mut value = status_json();
        value[field] = patch;
        assert!(
            AgentStatusUpdate::parse(value).is_err(),
            "{field} accepted an out-of-range value"
        );
    }
}

#[test]
fn the_message_is_bounded_in_utf8_bytes() {
    let mut value = status_json();
    value["message"] = serde_json::json!("x".repeat(AGENT_STATUS_MESSAGE_MAX_LENGTH + 1));
    assert!(AgentStatusUpdate::parse(value).is_err());
    let mut exact = status_json();
    exact["message"] = serde_json::json!("x".repeat(AGENT_STATUS_MESSAGE_MAX_LENGTH));
    assert!(AgentStatusUpdate::parse(exact).is_ok());
}

#[test]
fn an_unobserved_source_is_rejected() {
    let mut value = identified_json();
    value["source"] = serde_json::json!("worker");
    assert!(AgentStatus::parse(value).is_err());
}
