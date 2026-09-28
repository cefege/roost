//! The session-plane and terminal-metadata arms, decoded from the bytes the
//! coordinator sends and applied through `ClientCore::handle`: typed and JSON
//! session events with the recovery cursor, presence, last activity, agent
//! status, view state and input results.
//!
//! Ported from v2 `apps/web/src/store/sync-frame.ts:108-137,231-276` and
//! `apps/web/src/store/agent-status.ts:213-235`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::event::ClientEvent;
use roost_client_core::sync::decode::DecodeRefusal;
use roost_client_core::{SyncDomain, SyncFrame};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{
    AgentStatusFrame, InputRejected, JsonEvent, LastActivityFrame, SessionEventProto,
    SessionPresence, TerminalViewStateFrame, TerminalViewStatus,
};
use roost_protocol::wire::{
    ChannelId, SessionEvent, SessionId, SessionKind, WorkerFp, event_to_proto,
};

use sync_decode_support::{
    DOMAIN_GENERATION, SESSION, WORKER_FP, acked, application, closes, control, cursor, decoded,
    deliver, frame_of, ready_core, refused,
};

fn opened(cwd: &str) -> SessionEvent {
    SessionEvent::Opened {
        session_id: SessionId::try_from(SESSION).unwrap(),
        worker_fp: WorkerFp::try_from(WORKER_FP).unwrap(),
        channel: ChannelId::try_from(0_i64).unwrap(),
        session_kind: SessionKind::Shell,
        cwd: cwd.to_owned(),
        ts: 1,
        trace_id: None,
    }
}

fn moved_to(cwd: &str) -> SessionEvent {
    SessionEvent::Cwd {
        session_id: SessionId::try_from(SESSION).unwrap(),
        cwd: cwd.to_owned(),
        ts: 2,
        trace_id: None,
    }
}

/// The typed arm, built as `feed::frames::session_message_frame` builds it.
fn typed_event(event: &SessionEvent, event_id: u64) -> Frame {
    Frame::SessionEvent(Box::new(event_to_proto(event, event_id).unwrap()))
}

/// The legacy JSON arm: the wire event with its `_event_id`, as v2's feed wrote it.
fn json_event(event: &SessionEvent, event_id: u64) -> Frame {
    let mut payload = serde_json::to_value(event).unwrap();
    payload["_event_id"] = serde_json::json!(event_id);
    json_payload(&payload.to_string())
}

fn json_payload(payload_json: &str) -> Frame {
    Frame::Sessions(Box::new(JsonEvent {
        payload_json: payload_json.to_owned(),
        ..JsonEvent::default()
    }))
}

fn presence(payload_json: &str) -> Frame {
    Frame::SessionPresence(Box::new(SessionPresence {
        session_id: SESSION.to_owned(),
        payload_json: payload_json.to_owned(),
        ..SessionPresence::default()
    }))
}

fn session_cwd(core: &roost_client_core::ClientCore) -> Option<String> {
    let id = SessionId::try_from(SESSION).unwrap();
    core.store()
        .sessions
        .session(&id)
        .map(|row| row.cwd.clone())
}

#[test]
fn a_typed_session_event_folds_and_advances_the_cursor() {
    let (mut core, generation) = ready_core();
    let effects = deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 1, typed_event(&opened("/repo"), 40)),
    );
    assert_eq!(acked(&effects), vec![1]);
    assert_eq!(session_cwd(&core).as_deref(), Some("/repo"));
    assert_eq!(cursor(&mut core), 40);
}

#[test]
fn the_cursor_advances_only_to_a_greater_event_id() {
    // v2 `sync-frame.ts:115-118,139-142`: `_event_id > _lastSeenEventId`.
    let (mut core, generation) = ready_core();
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 1, typed_event(&opened("/a"), 50)),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 2, json_event(&moved_to("/b"), 20)),
    );
    assert_eq!(
        cursor(&mut core),
        50,
        "an older event id never rewinds the cursor"
    );
    assert_eq!(
        session_cwd(&core).as_deref(),
        Some("/b"),
        "the event itself is still folded; only the cursor is guarded"
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 3, json_event(&moved_to("/c"), 51)),
    );
    assert_eq!(cursor(&mut core), 51);
}

#[test]
fn unparseable_sessions_json_is_fatal_for_the_link() {
    let (mut core, generation) = ready_core();
    let refusal = refused(&application(
        SyncDomain::Terminal,
        1,
        json_payload("{not json"),
    ));
    assert!(
        matches!(
            refusal,
            DecodeRefusal::MalformedArm {
                arm: "sessions",
                ..
            }
        ),
        "{refusal}"
    );
    let effects = core.handle(ClientEvent::SyncFrameRefused {
        generation,
        reason: refusal.to_string(),
    });
    assert!(closes(&effects, generation), "{effects:?}");
    assert!(session_cwd(&core).is_none());
}

#[test]
fn sessions_json_that_is_not_a_session_event_moves_the_cursor_and_folds_nothing() {
    let (mut core, generation) = ready_core();
    let effects = deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Terminal,
            4,
            json_payload(r#"{"kind":"teleported","_event_id":77}"#),
        ),
    );
    assert_eq!(acked(&effects), vec![4], "the link stays up");
    assert_eq!(cursor(&mut core), 77);
    assert!(core.store().sessions.is_empty());
}

#[test]
fn a_typed_session_event_with_no_kind_is_refused() {
    let empty = Frame::SessionEvent(Box::new(SessionEventProto {
        event_id: 9,
        ..SessionEventProto::default()
    }));
    assert!(matches!(
        refused(&application(SyncDomain::Terminal, 1, empty)),
        DecodeRefusal::MalformedArm {
            arm: "session_event",
            ..
        }
    ));
}

#[test]
fn a_viewers_notice_replaces_the_viewer_list_and_other_presence_is_queued() {
    let (mut core, generation) = ready_core();
    deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Terminal,
            1,
            presence(
                r#"{"kind":"viewers","fps":["f1","f2"],"entries":[
                    {"fp":"f1","cols":120,"rows":40,"lastMs":5},
                    {"fp":"f2","cols":80,"rows":24,"viewerKey":"k2","label":"Phone"}]}"#,
            ),
        ),
    );
    let viewers = &core.store().session_viewers[SESSION];
    assert_eq!(viewers.len(), 2);
    assert_eq!(
        (
            viewers[0].viewer_key.as_str(),
            viewers[0].cols,
            viewers[0].last_ms
        ),
        ("f1", 120, Some(5)),
        "a viewer with no key is keyed by its fingerprint"
    );
    assert_eq!(
        (viewers[1].viewer_key.as_str(), viewers[1].label.as_deref()),
        ("k2", Some("Phone"))
    );
    deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Terminal,
            2,
            presence(r#"{"kind":"viewers","fps":["f3"]}"#),
        ),
    );
    let viewers = &core.store().session_viewers[SESSION];
    assert_eq!(
        (viewers.len(), viewers[0].fp.as_str(), viewers[0].cols),
        (1, "f3", 0)
    );

    deliver(
        &mut core,
        generation,
        &application(
            SyncDomain::Terminal,
            3,
            presence(r#"{"kind":"presence-delta","viewer_id":"v9"}"#),
        ),
    );
    let notice = core.store().presence_notices.back().expect("queued");
    assert_eq!(notice.session_id, SESSION);
    assert_eq!(notice.payload["viewer_id"], "v9");
}

#[test]
fn unparseable_presence_json_is_fatal_for_the_link() {
    assert!(matches!(
        refused(&application(SyncDomain::Terminal, 1, presence("["))),
        DecodeRefusal::MalformedArm {
            arm: "session_presence",
            ..
        }
    ));
}

#[test]
fn last_activity_is_kept_in_whole_milliseconds() {
    let (mut core, generation) = ready_core();
    let arm = Frame::LastActivity(Box::new(LastActivityFrame {
        session_id: SESSION.to_owned(),
        ts_ms: 1_700_000_000_123.0,
        ..LastActivityFrame::default()
    }));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 1, arm),
    );
    assert_eq!(
        core.store().last_activity_ms.get(SESSION),
        Some(&1_700_000_000_123)
    );
}

fn agent_status_arm(revision: u64, completed_revision: u64) -> Frame {
    Frame::AgentStatus(Box::new(AgentStatusFrame {
        session_id: SESSION.to_owned(),
        agent_id: "omp".to_owned(),
        state: "working".to_owned(),
        revision,
        completed_revision,
        updated_at: 1_700_000_000_000.0,
        active: true,
        ..AgentStatusFrame::default()
    }))
}

#[test]
fn an_agent_status_report_folds_and_a_refused_one_keeps_the_link() {
    let (mut core, generation) = ready_core();
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Terminal, 1, agent_status_arm(3, 0)),
    );
    let id = SessionId::try_from(SESSION).unwrap();
    assert_eq!(
        core.store()
            .agent_status
            .status(&id)
            .map(|status| status.common.revision),
        Some(3)
    );

    // completed_revision above revision fails the shared schema: v2 drops the
    // report and ignores the failure, so the frame is still acknowledged.
    let bad = application(SyncDomain::Terminal, 2, agent_status_arm(4, 9));
    assert!(matches!(
        frame_of(&decoded(&bad, generation)),
        SyncFrame::AgentStatusRefused { .. }
    ));
    let effects = deliver(&mut core, generation, &bad);
    assert_eq!(acked(&effects), vec![2]);
    assert_eq!(
        core.store()
            .agent_status
            .status(&id)
            .map(|status| status.common.revision),
        Some(3)
    );
}

#[test]
fn a_view_state_takes_the_envelope_generation_and_input_results_are_controls() {
    let view = Frame::TerminalViewState(Box::new(TerminalViewStateFrame {
        view_id: "view-1".to_owned(),
        session_id: SESSION.to_owned(),
        status: TerminalViewStatus::Accepted.into(),
        ..TerminalViewStateFrame::default()
    }));
    assert!(matches!(
        frame_of(&decoded(&application(SyncDomain::Terminal, 1, view), 1)),
        SyncFrame::ViewState {
            generation: DOMAIN_GENERATION,
            accepted: true,
            ..
        }
    ));
    let rejected = Frame::InputRejected(Box::new(InputRejected {
        session_id: SESSION.to_owned(),
        input_seq: 12,
        domain_generation: 2,
        reason: "route stale".to_owned(),
        ..InputRejected::default()
    }));
    assert!(matches!(
        frame_of(&decoded(&control(rejected), 1)),
        SyncFrame::InputResult {
            input_seq: 12,
            generation: 2,
            ..
        }
    ));
}
