//! The bounded reference contract, checked through the public surface a
//! persistence layer uses rather than through the module's own internals.

use serde_json::{Value, json};

use roost_protocol::agent_conversation_reference::{
    AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES,
    AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES, AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES,
    AgentConversationReferenceV1, is_agent_conversation_reference_event_envelope_bounded,
};

const SESSION: &str = "11111111-1111-4111-8111-111111111111";

fn reference_value() -> Value {
    json!({
        "schema_version": 1,
        "agent_id": "omp",
        "kind": "path",
        "value": "/tmp/a path/'$reference.json",
    })
}

#[test]
fn each_kind_is_bounded_separately() {
    let over_sized_id = "a".repeat(AGENT_CONVERSATION_SESSION_ID_MAX_UTF8_BYTES + 1);
    let mut id = reference_value();
    id["kind"] = json!("id");
    id["value"] = json!(over_sized_id);
    assert!(AgentConversationReferenceV1::parse(id).is_err());

    // The same bytes are legal inside a path, which is the larger bound.
    let mut path = reference_value();
    path["value"] = json!(format!("/{over_sized_id}"));
    assert!(AgentConversationReferenceV1::parse(path).is_ok());

    let mut too_long = reference_value();
    too_long["value"] = json!(format!(
        "/{}",
        "a".repeat(AGENT_CONVERSATION_SESSION_PATH_MAX_UTF8_BYTES)
    ));
    assert!(AgentConversationReferenceV1::parse(too_long).is_err());
}

#[test]
fn the_envelope_is_bounded_after_serialization_not_before() {
    let mut within_its_own_bound = reference_value();
    within_its_own_bound["value"] = json!(format!("/{}", "a".repeat(4_095)));
    let event = json!({
        "kind": "agent_reference",
        "session_id": SESSION,
        "reference": within_its_own_bound,
        "ts": 1,
    });
    assert!(is_agent_conversation_reference_event_envelope_bounded(
        &event
    ));

    // JSON escaping doubles each backslash, so a value inside its own bound
    // still pushes the stored envelope past the limit.
    let mut escaped = reference_value();
    escaped["value"] = json!(format!("/{}", "\\".repeat(4_095)));
    let event = json!({
        "kind": "agent_reference",
        "session_id": SESSION,
        "reference": escaped,
        "ts": 1,
    });
    assert!(!is_agent_conversation_reference_event_envelope_bounded(
        &event
    ));

    let mut traced = json!({
        "kind": "agent_reference",
        "session_id": SESSION,
        "reference": Value::Null,
        "ts": 1,
    });
    traced["trace_id"] = json!("a".repeat(AGENT_CONVERSATION_REFERENCE_EVENT_MAX_UTF8_BYTES));
    assert!(!is_agent_conversation_reference_event_envelope_bounded(
        &traced
    ));
}
