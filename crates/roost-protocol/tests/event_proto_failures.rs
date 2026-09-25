//! The two ways a frame fails to be an event must stay distinguishable: a kind
//! this build does not know is no event at all, and a kind it does know
//! arriving malformed is an error naming the field that broke.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_proto::__buffa::oneof::session_event_proto::Kind;
use roost_proto::{
    AgentReferenceEvt, ClosedEvt, OpenedEvt, PrEvt, Session as PbSession, SessionEventProto,
    SnapshotEvt, WorkspaceAssignedEvt,
};
use roost_protocol::agent_conversation_reference::{
    AgentConversationReferenceKind, AgentConversationReferenceV1,
};
use roost_protocol::proto_adapters::agent_conversation_reference_proto::agent_conversation_reference_to_proto;
use roost_protocol::wire::brand::{SessionId, TraceId};
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::event_proto::{event_to_proto, proto_to_event};

const SESSION_ID: &str = "00000000-0000-4000-8000-000000000abc";
const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const WORKSPACE_ID: &str = "00000000-0000-4000-8000-0000000000ab";

fn session_id() -> SessionId {
    SessionId::try_from(SESSION_ID).expect("fixture session id")
}

fn frame(kind: Kind) -> SessionEventProto {
    SessionEventProto {
        event_id: 4,
        kind: Some(kind),
        ..Default::default()
    }
}

fn opened(worker_fp: &str, session: &str, session_kind: &str, ts: u64) -> SessionEventProto {
    frame(Kind::Opened(Box::new(OpenedEvt {
        session_id: session.to_owned(),
        worker_fp: worker_fp.to_owned(),
        channel: 3,
        session_kind: session_kind.to_owned(),
        cwd: "/x".to_owned(),
        ts,
        ..Default::default()
    })))
}

fn field_of(proto: &SessionEventProto) -> String {
    proto_to_event(proto)
        .expect_err("a malformed known variant is an error, not a dropped frame")
        .field
}

fn reference() -> AgentConversationReferenceV1 {
    AgentConversationReferenceV1 {
        schema_version: 1,
        agent_id: "omp".to_owned(),
        kind: AgentConversationReferenceKind::Id,
        value: "conversation-1".to_owned(),
    }
}

#[test]
fn a_frame_with_no_kind_is_no_event() {
    let proto = SessionEventProto {
        event_id: 9,
        ..Default::default()
    };
    assert_eq!(
        proto_to_event(&proto).expect("no kind is not an error"),
        None
    );
}

#[test]
fn a_kind_this_build_does_not_know_is_no_event_rather_than_a_default() {
    // What a newer peer's frame looks like once this build's decoder has read
    // it: the event id survives, the unknown oneof member does not become a
    // variant, and nothing panics on the way.
    let newer_frame: SessionEventProto = serde_json::from_str(
        r#"{"eventId":42,"sessionTelemetry":{"opened":{"sessionId":"whatever"}}}"#,
    )
    .expect("an unknown oneof member is skipped, not refused");
    assert_eq!(newer_frame.event_id, 42);
    assert!(newer_frame.kind.is_none());
    assert_eq!(
        proto_to_event(&newer_frame).expect("no kind is not an error"),
        None
    );
}

#[test]
fn a_malformed_identity_names_the_field() {
    let short_fingerprint = "a".repeat(63);
    assert_eq!(
        field_of(&opened(&short_fingerprint, SESSION_ID, "shell", 1)),
        "session_event.opened.worker_fp"
    );
    assert_eq!(
        field_of(&opened(WORKER_FP, "not-a-uuid", "shell", 1)),
        "session_event.opened.session_id"
    );
    assert_eq!(
        field_of(&opened(WORKER_FP, SESSION_ID, "structured", 1)),
        "session_event.opened.session_kind"
    );
    let unassigned = frame(Kind::WorkspaceAssigned(Box::new(WorkspaceAssignedEvt {
        session_id: SESSION_ID.to_owned(),
        workspace_id: Some("nope".to_owned()),
        ts: 1,
        ..Default::default()
    })));
    assert_eq!(
        field_of(&unassigned),
        "session_event.workspace_assigned.workspace_id"
    );
}

#[test]
fn an_enum_value_this_build_does_not_know_is_an_error_not_a_default() {
    let proto = frame(Kind::Pr(Box::new(PrEvt {
        session_id: SESSION_ID.to_owned(),
        number: Some(1),
        state: Some("queued".to_owned()),
        ts: 1,
        ..Default::default()
    })));
    let error = proto_to_event(&proto).expect_err("an unknown pull request state is refused");
    assert_eq!(error.field, "session_event.pr.state");
    assert!(
        error.reason.contains("queued"),
        "the reason must name the value, got {}",
        error.reason
    );
}

#[test]
fn a_timestamp_outside_the_union_range_is_refused_in_both_directions() {
    assert_eq!(
        field_of(&opened(WORKER_FP, SESSION_ID, "shell", u64::MAX)),
        "session_event.opened.ts"
    );
    let before_epoch = frame(Kind::Closed(Box::new(ClosedEvt {
        session_id: SESSION_ID.to_owned(),
        ts: 0,
        ..Default::default()
    })));
    assert_eq!(field_of(&before_epoch), "session_event.closed.ts");

    let negative = SessionEvent::Closed {
        session_id: session_id(),
        exit_code: None,
        ts: -1,
        trace_id: None,
    };
    let error = event_to_proto(&negative, 1).expect_err("a negative timestamp cannot be encoded");
    assert_eq!(error.field, "session_event.closed.ts");
}

#[test]
fn a_malformed_session_inside_a_snapshot_names_its_index() {
    let mut sessions: Vec<PbSession> = (0..2)
        .map(|index| PbSession {
            id: SESSION_ID.to_owned(),
            worker_fp: WORKER_FP.to_owned(),
            channel: u32::try_from(index).expect("fixture channel"),
            kind: "shell".to_owned(),
            cwd: "/x".to_owned(),
            status: "open".to_owned(),
            created_at: 1,
            ..Default::default()
        })
        .collect();
    sessions[1].id = "not-a-uuid".to_owned();
    let proto = frame(Kind::Snapshot(Box::new(SnapshotEvt {
        worker_fp: WORKER_FP.to_owned(),
        sessions,
        ts: 1,
        ..Default::default()
    })));
    assert_eq!(field_of(&proto), "session_event.sessions[1].session_id");
}

#[test]
fn a_reference_event_past_its_envelope_bound_is_refused_in_both_directions() {
    let oversized = "a".repeat(8_192);
    let event = SessionEvent::AgentReference {
        session_id: session_id(),
        reference: Some(reference()),
        ts: 1,
        trace_id: Some(TraceId::try_from(oversized.clone()).expect("the value is hex")),
    };
    let error =
        event_to_proto(&event, 1).expect_err("an over-bound envelope must not reach the log");
    assert_eq!(error.field, "session_event.reference");

    let proto = frame(Kind::AgentReference(Box::new(AgentReferenceEvt {
        session_id: SESSION_ID.to_owned(),
        reference: agent_conversation_reference_to_proto(&reference())
            .expect("the fixture reference is valid")
            .into(),
        ts: 1,
        trace_id: Some(oversized),
        ..Default::default()
    })));
    assert_eq!(field_of(&proto), "session_event.reference");
}

#[test]
fn a_short_trace_id_is_refused_rather_than_kept_as_a_string() {
    let proto = frame(Kind::AgentReference(Box::new(AgentReferenceEvt {
        session_id: SESSION_ID.to_owned(),
        reference: agent_conversation_reference_to_proto(&reference())
            .expect("the fixture reference is valid")
            .into(),
        ts: 1,
        trace_id: Some("abc".to_owned()),
        ..Default::default()
    })));
    assert_eq!(field_of(&proto), "session_event.agent_reference.trace_id");
}

#[test]
fn a_valid_frame_decodes_and_keeps_its_event_id() {
    let proto = frame(Kind::WorkspaceAssigned(Box::new(WorkspaceAssignedEvt {
        session_id: SESSION_ID.to_owned(),
        workspace_id: Some(WORKSPACE_ID.to_owned()),
        ts: 1,
        ..Default::default()
    })));
    let decoded = proto_to_event(&proto)
        .expect("a valid workspace assignment decodes")
        .expect("the frame carries a kind");
    assert_eq!(decoded.event_id, 4);
    assert_eq!(decoded.event.ts(), 1);
}
