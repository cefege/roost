//! Contract tests for the session event ↔ protobuf boundary: every variant
//! survives it with its fields intact, and `git.remote` keeps the one
//! distinction the fold depends on.
//!
//! What the codecs lose on the way across — the 64-bit integers, the trace id
//! only one variant carries, and an omitted field against a written default —
//! is the other half, in `event_proto_precision.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_protocol::wire::brand::{ChannelId, WorkspaceId};
use roost_protocol::wire::event::{SessionEvent, fold_all, fold_event};
use roost_protocol::wire::session::{
    PullRequestChecks, PullRequestState, Session, SessionKind, SessionStatus,
};
use roost_protocol::wire::session_proto::{session_from_proto, session_to_proto};

use support::{
    fixture_session_id, git_event, opened_event, reference, round_trip, trace_id, worker_fp,
};

const WORKSPACE_ID: &str = "00000000-0000-4000-8000-0000000000ab";

fn session() -> Session {
    Session {
        id: fixture_session_id(),
        worker_fp: worker_fp(),
        channel: ChannelId::try_from(7_i64).expect("fixture channel"),
        kind: SessionKind::Shell,
        cwd: "/repo/subdir".to_owned(),
        spawn_cwd: Some("/repo".to_owned()),
        workspace_id: Some(WorkspaceId::try_from(WORKSPACE_ID).expect("fixture workspace")),
        status: SessionStatus::Closed,
        created_at: 1_781_500_000,
        closed_at: Some(1_781_500_100),
        custom_title: Some("release".to_owned()),
        git_branch: Some("main".to_owned()),
        git_remote: Some(Some("owner/repo".to_owned())),
        pr_number: Some(42),
        pr_state: Some(PullRequestState::Merged),
        pr_checks: Some(PullRequestChecks::Passing),
        pr_url: Some("https://github.com/owner/repo/pull/42".to_owned()),
        ports: Some(vec![3000, 5173]),
    }
}

fn remote_of(event: &SessionEvent) -> Option<Option<String>> {
    let projection = fold_all(&[opened_event(), event.clone()]);
    projection
        .get(&fixture_session_id())
        .and_then(|row| row.git_remote.clone())
}

#[test]
fn an_event_with_every_field_populated_survives_the_boundary() {
    let event = SessionEvent::Snapshot {
        worker_fp: worker_fp(),
        sessions: vec![session()],
        ts: 1_781_500_003,
        trace_id: None,
    };
    let decoded = round_trip(&event, 7);
    assert_eq!(decoded.event_id, 7);
    assert_eq!(decoded.event, event);
    let SessionEvent::Snapshot { sessions, .. } = &decoded.event else {
        panic!("a snapshot decodes as a snapshot");
    };
    let decoded_session = &sessions[0];
    assert_eq!(
        decoded_session.git_remote,
        Some(Some("owner/repo".to_owned()))
    );
    assert_eq!(decoded_session.pr_checks, Some(PullRequestChecks::Passing));
    assert_eq!(decoded_session.ports, Some(vec![3000, 5173]));
    assert_eq!(decoded_session.spawn_cwd.as_deref(), Some("/repo"));
}

#[test]
fn every_variant_survives_the_boundary() {
    let workspace = WorkspaceId::try_from(WORKSPACE_ID).expect("fixture workspace");
    let variants = [
        opened_event(),
        SessionEvent::Closed {
            session_id: fixture_session_id(),
            exit_code: Some(0),
            ts: 11,
            trace_id: None,
        },
        SessionEvent::Closed {
            session_id: fixture_session_id(),
            exit_code: None,
            ts: 12,
            trace_id: None,
        },
        SessionEvent::Attached {
            session_id: fixture_session_id(),
            ts: 13,
            trace_id: None,
        },
        SessionEvent::Detached {
            session_id: fixture_session_id(),
            ts: 14,
            trace_id: None,
        },
        SessionEvent::Cwd {
            session_id: fixture_session_id(),
            cwd: "/x/y".to_owned(),
            ts: 15,
            trace_id: None,
        },
        SessionEvent::WorkspaceAssigned {
            session_id: fixture_session_id(),
            workspace_id: Some(workspace.clone()),
            ts: 16,
            trace_id: None,
        },
        SessionEvent::WorkspaceAssigned {
            session_id: fixture_session_id(),
            workspace_id: None,
            ts: 17,
            trace_id: None,
        },
        SessionEvent::Snapshot {
            worker_fp: worker_fp(),
            sessions: Vec::new(),
            ts: 18,
            trace_id: None,
        },
        SessionEvent::Respawned {
            session_id: fixture_session_id(),
            new_channel: ChannelId::try_from(99_i64).expect("fixture channel"),
            ts: 19,
            trace_id: None,
        },
        SessionEvent::Renamed {
            session_id: fixture_session_id(),
            custom_title: "release".to_owned(),
            ts: 21,
            trace_id: None,
        },
        SessionEvent::Renamed {
            session_id: fixture_session_id(),
            custom_title: String::new(),
            ts: 22,
            trace_id: None,
        },
        git_event(Some("owner/repo"), 23),
        git_event(None, 24),
        SessionEvent::Pr {
            session_id: fixture_session_id(),
            number: Some(1481),
            state: Some(PullRequestState::Open),
            checks: Some(PullRequestChecks::Failing),
            url: Some("https://github.com/owner/repo/pull/1481".to_owned()),
            ts: 25,
            trace_id: None,
        },
        SessionEvent::Pr {
            session_id: fixture_session_id(),
            number: None,
            state: None,
            checks: None,
            url: None,
            ts: 26,
            trace_id: None,
        },
        SessionEvent::Ports {
            session_id: fixture_session_id(),
            ports: vec![5174, 8765],
            ts: 30,
            trace_id: None,
        },
        SessionEvent::Ports {
            session_id: fixture_session_id(),
            ports: Vec::new(),
            ts: 31,
            trace_id: None,
        },
        SessionEvent::AgentReference {
            session_id: fixture_session_id(),
            reference: Some(reference()),
            ts: 40,
            trace_id: Some(trace_id()),
        },
        SessionEvent::AgentReference {
            session_id: fixture_session_id(),
            reference: None,
            ts: 41,
            trace_id: None,
        },
    ];
    for event in &variants {
        let decoded = round_trip(event, 5);
        assert_eq!(decoded.event_id, 5);
        assert_eq!(&decoded.event, event);
    }
}

#[test]
fn git_remote_keeps_the_state_the_fold_depends_on() {
    // The event carries two states: absent, which means "leave the previously
    // resolved remote alone", and a value. A null one is not a state an
    // `optional string` field has, so it must not come back as an empty string.
    assert_eq!(round_trip(&git_event(None, 2), 1).event, git_event(None, 2));
    assert_eq!(
        round_trip(&git_event(Some("owner/repo"), 2), 1).event,
        git_event(Some("owner/repo"), 2)
    );

    let resolved = round_trip(&git_event(Some("owner/repo"), 2), 1).event;
    let unresolved = round_trip(&git_event(None, 3), 2).event;
    assert_eq!(
        remote_of(&resolved),
        Some(Some("owner/repo".to_owned())),
        "a resolved remote reaches the row"
    );
    let projection = fold_all(&[opened_event(), resolved.clone(), unresolved.clone()]);
    assert_eq!(
        projection
            .get(&fixture_session_id())
            .and_then(|row| row.git_remote.clone()),
        Some(Some("owner/repo".to_owned())),
        "a later git event with no remote must leave the resolved one alone"
    );
    // Absence never clears, in either order.
    let reversed = fold_event(&fold_all(&[opened_event(), unresolved]), &resolved);
    assert_eq!(
        reversed
            .get(&fixture_session_id())
            .and_then(|row| row.git_remote.clone()),
        Some(Some("owner/repo".to_owned()))
    );

    // The session row keeps three states. The wire has room for two, so a
    // present null collapses onto absent rather than onto a value.
    let absent = Session {
        git_remote: None,
        ..session()
    };
    let present_null = Session {
        git_remote: Some(None),
        ..session()
    };
    let present_value = Session {
        git_remote: Some(Some("owner/repo".to_owned())),
        ..session()
    };
    let wire_of = |row: &Session| session_to_proto(row).expect("the row encodes").git_remote;
    assert_eq!(wire_of(&absent), None);
    assert_eq!(wire_of(&present_null), None);
    assert_eq!(wire_of(&present_value).as_deref(), Some("owner/repo"));
    let decoded_of = |row: &Session| {
        session_from_proto(&session_to_proto(row).expect("the row encodes"))
            .expect("the row decodes")
            .git_remote
    };
    assert_eq!(decoded_of(&absent), None);
    assert_eq!(decoded_of(&present_null), None);
    assert_eq!(
        decoded_of(&present_value),
        Some(Some("owner/repo".to_owned()))
    );
}
