//! The values an event-core test asserts on: the ids the protocol's brands
//! require, and the event shapes the append path is exercised with. Nothing here
//! touches a database -- these are arguments a test builds and hands to the
//! fixture in `super::fixture`, so this file owns no state.

#![allow(dead_code)]
// Every expect here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_protocol::wire::{SessionEvent, SessionId, WorkerFp, WorkspaceId};

// The parent's index, for the same reason `fixture.rs` imports through it: one
// path to a name, so the two halves cannot drift into a module cycle.
use super::Caller;

/// The dashboard every fixture row is scoped to.
pub const DASHBOARD_ID: &str = "00000000-0000-4000-8000-0000000000d1";
/// The organization that owns it.
pub const ORGANIZATION_ID: &str = "00000000-0000-4000-8000-0000000000c1";

/// A worker fingerprint: 64 lowercase hex characters.
pub fn fingerprint(byte: char) -> WorkerFp {
    WorkerFp::try_from(byte.to_string().repeat(64)).expect("a repeated hex digit is a fingerprint")
}

/// A session id: a UUID, because the brand checks the shape.
pub fn session_id(last: char) -> SessionId {
    SessionId::try_from(format!("00000000-0000-4000-8000-00000000000{last}"))
        .expect("the fixture id is a UUID")
}

/// A workspace id: a UUID, for the same reason.
pub fn workspace_id(last: char) -> WorkspaceId {
    WorkspaceId::try_from(format!("00000000-0000-4000-8000-00000000001{last}"))
        .expect("the fixture id is a UUID")
}

/// An `opened` event on one channel.
pub fn opened_event(session: &SessionId, worker_fp: &WorkerFp, channel: i64) -> SessionEvent {
    SessionEvent::Opened {
        session_id: session.clone(),
        worker_fp: worker_fp.clone(),
        channel: roost_protocol::wire::ChannelId::try_from(channel)
            .expect("the fixture channel fits"),
        session_kind: roost_protocol::wire::SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        ts: 1,
        trace_id: None,
    }
}

/// A `respawned` event onto a new keeper channel.
pub fn respawned_event(session: &SessionId, channel: i64) -> SessionEvent {
    SessionEvent::Respawned {
        session_id: session.clone(),
        new_channel: roost_protocol::wire::ChannelId::try_from(channel)
            .expect("the fixture channel fits"),
        ts: 2,
        trace_id: None,
    }
}

/// A `closed` event: the terminal exited.
pub fn closed_event(session: &SessionId) -> SessionEvent {
    SessionEvent::Closed {
        session_id: session.clone(),
        exit_code: Some(0),
        ts: 3,
        trace_id: None,
    }
}

/// The live row a snapshot announces for one session.
pub fn live_session(
    session: &SessionId,
    worker_fp: &WorkerFp,
    channel: i64,
    workspace: Option<&WorkspaceId>,
) -> roost_protocol::wire::Session {
    roost_protocol::wire::Session {
        id: session.clone(),
        worker_fp: worker_fp.clone(),
        channel: roost_protocol::wire::ChannelId::try_from(channel)
            .expect("the fixture channel fits"),
        kind: roost_protocol::wire::SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        spawn_cwd: None,
        workspace_id: workspace.cloned(),
        status: roost_protocol::wire::SessionStatus::Open,
        created_at: 1_000,
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

/// A `snapshot` event announcing a worker's whole live set.
pub fn snapshot_event(
    worker_fp: &WorkerFp,
    sessions: Vec<roost_protocol::wire::Session>,
) -> SessionEvent {
    SessionEvent::Snapshot {
        worker_fp: worker_fp.clone(),
        sessions,
        ts: 5_000,
        trace_id: None,
    }
}

/// A worker caller on one outbox sequence.
pub fn worker_caller(worker_fp: &WorkerFp, client_seq: u64) -> Caller {
    Caller::worker(worker_fp.clone(), client_seq, DASHBOARD_ID)
}