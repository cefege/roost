//! `SessionsSpawn` end to end against a recording worker socket: the one worker
//! command, the answer it settles, the joins and refusals around it, and how a
//! lost, superseded or revoked worker link resolves the reservation.
//!
//! Ports the spawn case of `apps/coord/tests/coord-bidi.test.ts`, the spawn
//! half of `apps/coord/tests/workers/worker-conn-pending-supersession.test.ts`,
//! and the handler paths `apps/coord/tests/sessions/pending-spawns.test.ts`
//! reaches through the table.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sessions_support;

use connectrpc::{ConnectError, ErrorCode};
use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::worker_lifecycle::LinkEnd;
use roost_coord::sessions::spawn::handle_sessions_spawn;
use roost_proto::{SessionsSpawnRequest, SessionsSpawnResponse};
use roost_protocol::wire::control::ClientControlFrame;
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, SessionKind, WorkerFp};
use serde_json::json;
use sessions_support::{BROWSER_FP, SessionsHarness, TAB, WORKER_FP, session};
use tokio::task::JoinHandle;

fn request(session_id: &str) -> SessionsSpawnRequest {
    SessionsSpawnRequest {
        worker_fp: WORKER_FP.to_owned(),
        kind: "shell".to_owned(),
        folder: "/tmp".to_owned(),
        cols: Some(80),
        rows: Some(24),
        session_id: Some(session_id.to_owned()),
        ..Default::default()
    }
}

fn spawn(
    harness: &SessionsHarness,
    req: SessionsSpawnRequest,
) -> JoinHandle<Result<SessionsSpawnResponse, ConnectError>> {
    let core = harness.core.clone();
    let caller = harness.device(Some(TAB));
    tokio::spawn(async move {
        handle_sessions_spawn(&core, &caller, req)
            .await
            .map(|response| response.body)
    })
}

fn opened(session_id: &str, channel: i64) -> SessionEvent {
    SessionEvent::Opened {
        session_id: SessionId::try_from(session_id).unwrap(),
        worker_fp: WorkerFp::try_from(WORKER_FP).unwrap(),
        channel: ChannelId::try_from(channel).unwrap(),
        session_kind: SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        ts: 1,
        trace_id: None,
    }
}

/// Let the detached reply-settling task run to completion, so an assertion that
/// the spawn is still pending cannot pass merely because it has not run yet.
async fn settle_background_tasks() {
    for _ in 0..32 {
        tokio::task::yield_now().await;
    }
}

// v2 coord-bidi: "SessionsSpawn with no worker attached → FAILED_PRECONDITION".
#[tokio::test]
async fn a_registered_worker_with_no_link_refuses_the_spawn() {
    let harness = SessionsHarness::new("spawn-offline").await;
    let error = spawn(&harness, request(&session("1")))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::FailedPrecondition);
    assert_eq!(
        error.message.as_deref(),
        Some("worker 5e5500000000 not connected")
    );
    // The definite failure released the UUID for the next attempt.
    harness.connect_worker();
    let retry = spawn(&harness, request(&session("1")));
    let command = harness.command(1).await;
    harness.reply_ok(
        &command.request_id,
        json!({ "session_id": session("1"), "channel_id": 4 }),
    );
    assert_eq!(retry.await.unwrap().unwrap().channel_id, 4);
}

#[tokio::test]
async fn one_spawn_command_is_relayed_and_its_reply_is_the_answer() {
    let harness = SessionsHarness::new("spawn-relay").await;
    harness.connect_worker();
    let sid = session("2");
    let pending = spawn(&harness, request(&sid));
    let command = harness.command(1).await;
    assert_eq!(command.browser_id, BROWSER_FP);
    assert_eq!(command.viewer_id, format!("{BROWSER_FP}:{TAB}"));
    match &command.frame {
        ClientControlFrame::SpawnShell {
            folder,
            cols,
            rows,
            session_id,
            ..
        } => {
            assert_eq!(folder, "/tmp");
            assert_eq!((*cols, *rows), (Some(80), Some(24)));
            assert_eq!(
                session_id.as_ref().map(SessionId::as_str),
                Some(sid.as_str())
            );
        }
        other => panic!("expected spawn-shell, got {other:?}"),
    }
    harness.reply_ok(
        &command.request_id,
        json!({ "session_id": sid, "channel_id": 7 }),
    );
    let answer = pending.await.unwrap().unwrap();
    assert_eq!(
        (answer.session_id.as_str(), answer.channel_id),
        (sid.as_str(), 7)
    );
}

// v2 pending-spawns: exact duplicates share one result; mismatches conflict
// before a second worker command can create an orphan PTY.
#[tokio::test]
async fn an_exact_retry_joins_and_a_changed_retry_conflicts() {
    let harness = SessionsHarness::new("spawn-join").await;
    harness.connect_worker();
    let sid = session("3");
    let first = spawn(&harness, request(&sid));
    let command = harness.command(1).await;
    let joined = spawn(&harness, request(&sid));
    let wider = SessionsSpawnRequest {
        cols: Some(81),
        ..request(&sid)
    };
    let conflict = spawn(&harness, wider).await.unwrap().unwrap_err();
    assert_eq!(conflict.code, ErrorCode::AlreadyExists);
    harness.reply_ok(
        &command.request_id,
        json!({ "session_id": sid, "channel_id": 9 }),
    );
    assert_eq!(first.await.unwrap().unwrap().channel_id, 9);
    assert_eq!(joined.await.unwrap().unwrap().channel_id, 9);
    assert_eq!(harness.commands().len(), 1, "one UUID, one worker command");
}

#[tokio::test]
async fn a_reply_that_names_another_identity_is_data_loss() {
    let harness = SessionsHarness::new("spawn-identity").await;
    harness.connect_worker();
    let pending = spawn(&harness, request(&session("4")));
    let command = harness.command(1).await;
    harness.reply_ok(
        &command.request_id,
        json!({ "session_id": session("5"), "channel_id": 3 }),
    );
    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::DataLoss);

    let zero = spawn(&harness, request(&session("6")));
    let command = harness.command(2).await;
    harness.reply_ok(
        &command.request_id,
        json!({ "session_id": session("6"), "channel_id": 0 }),
    );
    assert_eq!(zero.await.unwrap().unwrap_err().code, ErrorCode::DataLoss);
}

#[tokio::test]
async fn a_worker_command_failure_is_definite() {
    let harness = SessionsHarness::new("spawn-rpc-error").await;
    harness.connect_worker();
    let pending = spawn(&harness, request(&session("7")));
    let command = harness.command(1).await;
    harness.reply_error(&command.request_id, "keeper rejected spawn");
    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    assert_eq!(error.message.as_deref(), Some("keeper rejected spawn"));
}

// v2 A5 + pending-spawns: a dropped link fails the worker reply as Unavailable,
// which is ambiguous, so the durable `opened` still answers the caller.
#[tokio::test]
async fn a_reply_lost_to_a_closed_link_is_answered_by_the_durable_opened() {
    let harness = SessionsHarness::new("spawn-closed").await;
    let handle = harness.connect_worker();
    let sid = session("8");
    let pending = spawn(&harness, request(&sid));
    harness.command(1).await;
    let lifecycle = &harness.core.services.worker_lifecycle;
    lifecycle.closed(&handle, LinkEnd::Closed { replaced: false });
    settle_background_tasks().await;
    assert!(!pending.is_finished(), "an ambiguous failure keeps waiting");
    let sessions = &harness.core.services.sessions;
    assert!(sessions.resolve_spawn_on_opened(&handle.worker_fp, &opened(&sid, 12)));
    assert_eq!(pending.await.unwrap().unwrap().channel_id, 12);
}

// v2 worker-conn-pending-supersession: a replacement generation rejects the
// predecessor's in-flight RPC; for a spawn that is ambiguous, not a failure.
#[tokio::test]
async fn a_superseded_generation_leaves_the_spawn_to_the_durable_opened() {
    let harness = SessionsHarness::new("spawn-superseded").await;
    let old = harness.connect_worker();
    let sid = session("9");
    let pending = spawn(&harness, request(&sid));
    harness.command(1).await;
    let replacement = harness.connect_worker();
    harness.core.services.worker_lifecycle.superseded(&old);
    settle_background_tasks().await;
    assert!(
        !pending.is_finished(),
        "a supersede is not a definite failure"
    );
    let sessions = &harness.core.services.sessions;
    assert!(sessions.resolve_spawn_on_opened(&replacement.worker_fp, &opened(&sid, 2)));
    assert_eq!(pending.await.unwrap().unwrap().channel_id, 2);
}

// v2 worker-conn.ts revoke(): rejectPendingSpawnsForWorker.
#[tokio::test]
async fn a_revoked_worker_credential_rejects_its_pending_spawn() {
    let harness = SessionsHarness::new("spawn-revoked").await;
    let handle = harness.connect_worker();
    let pending = spawn(&harness, request(&session("a")));
    harness.command(1).await;
    harness
        .core
        .services
        .worker_lifecycle
        .closed(&handle, LinkEnd::Revoked);
    let error = pending.await.unwrap().unwrap_err();
    assert_eq!(error.code, ErrorCode::Unauthenticated);
    assert_eq!(error.message.as_deref(), Some("worker credential revoked"));
}

#[tokio::test]
async fn invalid_spawns_are_refused_before_any_worker_command() {
    let harness = SessionsHarness::new("spawn-refusals").await;
    harness.connect_worker();
    harness.seed_session(&session("b"), 1).await;
    let refusals = [
        (request("not-a-uuid"), ErrorCode::InvalidArgument),
        (
            SessionsSpawnRequest {
                worker_fp: "ff".repeat(32),
                ..request(&session("c"))
            },
            ErrorCode::NotFound,
        ),
        (request(&session("b")), ErrorCode::AlreadyExists),
        (
            SessionsSpawnRequest {
                kind: "agent".to_owned(),
                ..request(&session("d"))
            },
            ErrorCode::InvalidArgument,
        ),
    ];
    for (req, code) in refusals {
        let error = spawn(&harness, req).await.unwrap().unwrap_err();
        assert_eq!(error.code, code, "{error:?}");
    }
    let mut worker = harness.device(None);
    worker.principal = Principal::Worker {
        fingerprint: WORKER_FP.to_owned(),
        label: "laptop".to_owned(),
    };
    let refused = handle_sessions_spawn(&harness.core, &worker, request(&session("e"))).await;
    assert_eq!(refused.unwrap_err().code, ErrorCode::Unauthenticated);
    assert!(
        harness.commands().is_empty(),
        "no refusal reached the worker"
    );
}
