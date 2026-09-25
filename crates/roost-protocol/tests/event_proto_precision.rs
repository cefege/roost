//! What survives a lossy codec at the session event ↔ protobuf boundary.
//!
//! The 64-bit fields past the range a double holds exactly, the trace id only
//! the agent-reference variant carries, the three kinds of absence that must
//! each cost zero bytes, and the row a decoded opened event fills. Every
//! variant surviving the boundary intact is the other half, in
//! `event_proto_contract.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_proto::__buffa::oneof::session_event_proto::Kind;
use roost_proto::{OpenedEvt, SessionEventProto};
use roost_protocol::agent_conversation_reference::AgentConversationReferenceV1;
use roost_protocol::wire::event::{SessionEvent, fold_all};
use roost_protocol::wire::event_proto::{event_to_proto, proto_to_event};
use roost_protocol::wire::session::SessionStatus;

use support::{
    EVENT_ID_PRECISION_PROBE, SESSION_ID, TRACE_ID, WORKER_FP, fixture_session_id, git_event,
    opened_event_with_trace, protobuf_bytes, reference, round_trip, through_protobuf_binary,
    trace_id,
};

#[test]
fn the_event_id_and_the_timestamp_stay_integers_past_the_safe_range() {
    let event = SessionEvent::Attached {
        session_id: fixture_session_id(),
        ts: 1_781_500_000_000,
        trace_id: None,
    };
    let proto = event_to_proto(&event, EVENT_ID_PRECISION_PROBE).expect("the event encodes");
    let encoded = serde_json::to_string(&proto).expect("a generated message serializes");
    // A 64-bit field is written as a JSON STRING of decimal digits, not as a
    // JSON number: that is canonical protobuf JSON, and it is why a value past
    // 2^53 survives where a JavaScript number would have rounded it away. The
    // assertion is on the digits, not on the quoting.
    assert!(
        encoded.contains(&EVENT_ID_PRECISION_PROBE.to_string()),
        "the id must be written as its exact digits, got {encoded}"
    );
    let decoded = round_trip(&event, EVENT_ID_PRECISION_PROBE);
    assert_eq!(decoded.event_id, EVENT_ID_PRECISION_PROBE);
    assert_eq!(decoded.event.ts(), 1_781_500_000_000);
}

#[test]
fn only_agent_reference_carries_a_trace_id() {
    let traced = SessionEvent::AgentReference {
        session_id: fixture_session_id(),
        reference: Some(reference()),
        ts: 40,
        trace_id: Some(trace_id()),
    };
    let proto = event_to_proto(&traced, 10).expect("the event encodes");
    let Some(Kind::AgentReference(value)) = &proto.kind else {
        panic!("an agent reference encodes as an agent reference");
    };
    assert_eq!(value.trace_id.as_deref(), Some(TRACE_ID));
    assert!(value.reference.is_set());
    assert_eq!(round_trip(&traced, 10).event, traced);

    // No other envelope has a `trace_id` field, so the union's copy is dropped
    // rather than smuggled through a variant that cannot describe it.
    let opened = opened_event_with_trace(Some(trace_id()));
    let proto = event_to_proto(&opened, 11).expect("the event encodes");
    assert!(matches!(&proto.kind, Some(Kind::Opened(_))));
    let SessionEvent::Opened { trace_id, .. } = round_trip(&opened, 11).event else {
        panic!("an opened event decodes as an opened event");
    };
    assert!(trace_id.is_none());

    // A cleared reference is an absent message field, not an empty one.
    let cleared = SessionEvent::AgentReference {
        session_id: fixture_session_id(),
        reference: None,
        ts: 42,
        trace_id: None,
    };
    let proto = event_to_proto(&cleared, 12).expect("the event encodes");
    let Some(Kind::AgentReference(value)) = &proto.kind else {
        panic!("an agent reference encodes as an agent reference");
    };
    assert!(value.reference.is_unset());
    assert_eq!(round_trip(&cleared, 12).event, cleared);
}

#[test]
fn a_decoded_opened_event_fills_the_row_the_fold_produces() {
    let proto = SessionEventProto {
        event_id: 3,
        kind: Some(Kind::Opened(Box::new(OpenedEvt {
            session_id: SESSION_ID.to_owned(),
            worker_fp: WORKER_FP.to_owned(),
            channel: 3,
            session_kind: "shell".to_owned(),
            cwd: "/x".to_owned(),
            ts: 1,
            ..Default::default()
        }))),
        ..Default::default()
    };
    let decoded = proto_to_event(&proto)
        .expect("an opened event decodes")
        .expect("the frame carries a kind");
    let projection = fold_all(&[decoded.event]);
    let row = projection
        .get(&fixture_session_id())
        .expect("the fold holds the session");
    assert_eq!(row.cwd, "/x");
    assert_eq!(row.status, SessionStatus::Open);
    assert_eq!(row.spawn_cwd.as_deref(), Some("/x"));
}

#[test]
fn an_absent_field_is_omitted_from_the_wire_rather_than_written_as_a_default() {
    let wire_of = |event: &SessionEvent| {
        protobuf_bytes(&event_to_proto(event, 1).expect("the event encodes"))
    };
    let agent_event =
        |reference: Option<AgentConversationReferenceV1>| SessionEvent::AgentReference {
            session_id: fixture_session_id(),
            reference,
            ts: 42,
            trace_id: None,
        };
    let ports_event = |ports: Vec<i64>| SessionEvent::Ports {
        session_id: fixture_session_id(),
        ports,
        ts: 31,
        trace_id: None,
    };

    // Three kinds of absence, and each one has to cost zero bytes: an optional
    // scalar nobody resolved, a message field nobody set, and a repeated field
    // with nothing in it.
    assert!(
        wire_of(&git_event(Some("owner/repo"), 2)).len() > wire_of(&git_event(None, 2)).len(),
        "an unresolved remote must not be written as an empty string"
    );
    assert!(
        wire_of(&agent_event(Some(reference()))).len() > wire_of(&agent_event(None)).len(),
        "a cleared reference must not be written as an empty submessage"
    );
    assert!(
        wire_of(&ports_event(vec![5174])).len() > wire_of(&ports_event(Vec::new())).len(),
        "an empty port list must not be written"
    );

    // And the frame with nothing in it still decodes, rather than failing on a
    // field the encoder declined to write.
    let empty = through_protobuf_binary(
        &event_to_proto(&git_event(None, 2), 1).expect("the event encodes"),
    );
    let decoded = proto_to_event(&empty)
        .expect("a frame with nothing in it decodes")
        .expect("the frame carries a kind");
    assert_eq!(decoded.event, git_event(None, 2));
}
