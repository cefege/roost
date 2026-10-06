//! Terminal input and input-route controls over a real Sync socket: a batch
//! reaches the worker with its authenticated actor and its keeper-proven
//! outcome returns on the control lane with a durable audit row; claims and
//! probes are admitted only inside the socket's live scope; a closing socket
//! retires the routes it claimed. Ports `apps/coord/tests/sync/sync-terminal-route-controls.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_coord::terminal_screen::typed_results::TypedWorkerResult;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::{
    FirehoseFrame, InputCommand, SyncDomain, SyncDomainReadyCommand, TerminalInputRouteClaim,
    TerminalInputRouteResult, TerminalTransportProbe, WTerminalInputRouteResult,
};
use roost_protocol::versioning::CAPABILITY_TERMINAL_INPUT_ROUTE_V1;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, InputResult, TerminalInputStatus, TerminalWritePhase,
};
use roost_protocol::wire::{SessionId, WorkerFp};
use sqlx::AssertSqlSafe;
use sync_ws_socket_support::{
    EXPECT, SyncFixture, generation_of, next_firehose, read_subscribed, send_client_frame,
};
use ws_client_support::WsClient;

const WORKER_FP: &str = "aa00000000000000000000000000000000000000000000000000000000000000";
const WORKER_EPOCH: &str = "worker-epoch-a";
const SESSION: &str = "00000000-0000-4000-8000-000000000001";
const TAB: &str = "tab-e2e";

type SentFrames = Arc<Mutex<Vec<CoordWorkerDownstream>>>;

/// A socket whose terminal domain is hydrated over one open session.
struct Hydrated {
    fixture: SyncFixture,
    socket: WsClient,
    socket_id: String,
    fingerprint: String,
    generation: u64,
    worker: Arc<WorkerHandle>,
    sent: SentFrames,
}

async fn hydrated(label: &str) -> Hydrated {
    let fixture = SyncFixture::start(label).await;
    let (fingerprint, token) = fixture.enroll_browser(9).await;
    let dashboard_id = fixture
        .services
        .boot
        .tenant
        .as_ref()
        .unwrap()
        .dashboard_id
        .clone();
    for (sql, bind) in [
        (
            "INSERT INTO workers (fp, label, os, registered_at_ms, last_seen_ms, dashboard_id) \
             VALUES ($1, 'laptop', 'linux', 0, 0, $2)",
            WORKER_FP,
        ),
        (
            "INSERT INTO sessions (id, dashboard_id, worker_fp, channel, kind, cwd, status, created_at) \
             VALUES ($1, $2, 'aa00000000000000000000000000000000000000000000000000000000000000', 1, 'shell', '/tmp', 'open', 0)",
            SESSION,
        ),
    ] {
        sqlx::query(AssertSqlSafe(sql))
            .bind(bind)
            .bind(&dashboard_id)
            .execute(fixture.services.db.pool())
            .await
            .unwrap();
    }
    let sent = SentFrames::default();
    let worker = attach_worker(&fixture, &sent);
    let mut socket = fixture
        .dial_sync(&format!("flow=1&sync_v=2&tab={TAB}"), &token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut socket).await;
    let generation = generation_of(&subscribed, SyncDomain::Terminal);
    let snapshot = fixture
        .services
        .feed
        .bind_session_snapshot(
            &subscribed.socket_id,
            &fingerprint,
            BTreeSet::from([SESSION.to_owned()]),
        )
        .unwrap();
    let ready = Command::DomainReady(Box::new(SyncDomainReadyCommand {
        domain: SyncDomain::Terminal.into(),
        generation,
        snapshot_token: Some(snapshot),
        ..SyncDomainReadyCommand::default()
    }));
    send_client_frame(&mut socket, &subscribed.socket_id, None, Some(ready)).await;
    Hydrated {
        fixture,
        socket,
        socket_id: subscribed.socket_id,
        fingerprint,
        generation,
        worker,
        sent,
    }
}

/// A ready, route-capable worker generation that proves every input written.
fn attach_worker(fixture: &SyncFixture, sent: &SentFrames) -> Arc<WorkerHandle> {
    let sent = Arc::clone(sent);
    let pending = Arc::clone(fixture.services.scrollback.pending());
    let handle = WorkerHandle::new(
        WorkerFp::try_from(WORKER_FP).unwrap(),
        Some(WORKER_EPOCH.to_owned()),
        "worker-connection-a".to_owned(),
        BTreeSet::from([CAPABILITY_TERMINAL_INPUT_ROUTE_V1.to_owned()]),
        Arc::new(move |frame: CoordWorkerDownstream| {
            if let CoordWorkerDownstream::InputRequest(request) = &frame {
                let result = InputResult {
                    request_id: request.request_id.clone(),
                    session_id: SessionId::try_from(request.session_id.as_str()).unwrap(),
                    input_seq: request.input_seq,
                    status: TerminalInputStatus::Accepted,
                    written_bytes: u32::try_from(request.data.len()).unwrap(),
                    reason: String::new(),
                    phase: TerminalWritePhase::Written,
                };
                pending.resolve_typed(TypedWorkerResult::Input(result), Some(WORKER_FP));
            }
            sent.lock().unwrap().push(frame);
            1
        }),
    );
    handle.mark_ready();
    let handle = Arc::new(handle);
    fixture.services.workers.insert(Arc::clone(&handle));
    handle
}

/// The next control answer, skipping application traffic and keepalives.
async fn next_answer(socket: &mut WsClient) -> Frame {
    loop {
        let frame: FirehoseFrame = next_firehose(socket, EXPECT).await.expect("an answer");
        match frame.frame {
            Some(
                answer @ (Frame::InputAccepted(_)
                | Frame::InputRejected(_)
                | Frame::InputAmbiguous(_)
                | Frame::InputRouteResult(_)
                | Frame::TerminalTransportProbeResult(_)),
            ) => {
                assert_eq!(frame.delivery_seq, 0, "an answer rides the control lane");
                return answer;
            }
            _ => continue,
        }
    }
}

async fn wait_for_sent(
    sent: &SentFrames,
    matches: impl Fn(&CoordWorkerDownstream) -> bool,
) -> CoordWorkerDownstream {
    for _ in 0..500 {
        if let Some(frame) = sent.lock().unwrap().iter().find(|frame| matches(frame)) {
            return frame.clone();
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the worker was never sent the expected frame");
}

fn input(session_id: &str, generation: u64) -> Command {
    Command::Input(Box::new(InputCommand {
        session_id: session_id.to_owned(),
        input_seq: 1,
        data: b"ls\r".to_vec(),
        domain_generation: generation,
        ..InputCommand::default()
    }))
}

fn claim(session_id: &str, generation: u64) -> Command {
    Command::InputRouteClaim(Box::new(TerminalInputRouteClaim {
        request_id: "claim-e2e".to_owned(),
        session_id: session_id.to_owned(),
        revision: 1,
        domain_generation: generation,
        worker_epoch: WORKER_EPOCH.to_owned(),
        ..TerminalInputRouteClaim::default()
    }))
}

// v2 sync-terminal-controls.ts handleSyncInput: accepted batches answer
// `inputAccepted` with the command's generation, after the audit row lands.
#[tokio::test]
async fn a_sync_batch_reaches_the_worker_and_its_audited_acceptance_returns() {
    let mut hydrated = hydrated("input-accepted").await;
    let (socket_id, generation) = (hydrated.socket_id.clone(), hydrated.generation);
    send_client_frame(
        &mut hydrated.socket,
        &socket_id,
        None,
        Some(input(SESSION, generation)),
    )
    .await;

    let Frame::InputAccepted(accepted) = next_answer(&mut hydrated.socket).await else {
        panic!("expected inputAccepted");
    };
    assert_eq!(
        (accepted.session_id.as_str(), accepted.input_seq),
        (SESSION, 1)
    );
    assert_eq!(
        (accepted.domain_generation, accepted.written_bytes),
        (generation, 3)
    );
    let CoordWorkerDownstream::InputRequest(request) = wait_for_sent(&hydrated.sent, |frame| {
        matches!(frame, CoordWorkerDownstream::InputRequest(_))
    })
    .await
    else {
        unreachable!("filtered to an input request");
    };
    assert_eq!(request.device_fingerprint, hydrated.fingerprint);
    assert_eq!(request.tab_id, TAB);
    assert_eq!(request.browser_connection_id, socket_id);
    let audited: Vec<(String, i64)> =
        sqlx::query_as("SELECT path, status FROM audit_log WHERE caller_fp = $1")
            .bind(&hydrated.fingerprint)
            .fetch_all(hydrated.fixture.services.db.pool())
            .await
            .unwrap();
    assert_eq!(
        audited,
        vec![(
            "/ws/coord-sync/input/accepted/3/SessionsInput".to_owned(),
            200
        )]
    );
}

// v2: "does not claim a session outside the authenticated socket scope", and
// the same fence for input.
#[tokio::test]
async fn a_session_outside_the_socket_scope_is_refused_for_input_and_claims() {
    let mut hydrated = hydrated("out-of-scope").await;
    let (socket_id, generation) = (hydrated.socket_id.clone(), hydrated.generation);
    let foreign = "00000000-0000-4000-8000-0000000000ff";

    send_client_frame(
        &mut hydrated.socket,
        &socket_id,
        None,
        Some(input(foreign, generation)),
    )
    .await;
    let Frame::InputRejected(rejected) = next_answer(&mut hydrated.socket).await else {
        panic!("expected inputRejected");
    };
    assert_eq!(rejected.reason, "terminal session is unavailable");

    send_client_frame(
        &mut hydrated.socket,
        &socket_id,
        None,
        Some(claim(foreign, generation)),
    )
    .await;
    let Frame::InputRouteResult(result) = next_answer(&mut hydrated.socket).await else {
        panic!("expected an input route result");
    };
    assert!(!result.accepted);
    assert_eq!(result.request_id, "claim-e2e");
    assert_eq!(result.reason, "terminal session is unavailable");
    assert!(
        hydrated.sent.lock().unwrap().is_empty(),
        "nothing reached the worker"
    );
}

// v2: "does not probe a worker outside the authenticated socket scope"
#[tokio::test]
async fn a_probe_of_a_worker_outside_the_socket_scope_is_answered_empty() {
    let mut hydrated = hydrated("foreign-probe").await;
    let socket_id = hydrated.socket_id.clone();
    let probe = Command::TerminalTransportProbe(Box::new(TerminalTransportProbe {
        request_id: "probe-foreign-worker".to_owned(),
        worker_fp: "foreign-worker".to_owned(),
        ..TerminalTransportProbe::default()
    }));

    send_client_frame(&mut hydrated.socket, &socket_id, None, Some(probe)).await;

    let Frame::TerminalTransportProbeResult(result) = next_answer(&mut hydrated.socket).await
    else {
        panic!("expected a transport probe result");
    };
    assert_eq!(result.request_id, "probe-foreign-worker");
    assert_eq!(result.worker_fp, "foreign-worker");
    assert!(result.worker_epoch.is_empty());
}

// v2 onV2Close: a claimed route is answered with the worker's validated result
// under the browser's own nonce, and the socket's close retires it on the worker.
#[tokio::test]
async fn a_claimed_route_is_answered_and_retired_when_its_socket_closes() {
    let mut hydrated = hydrated("claim-close").await;
    let (socket_id, generation) = (hydrated.socket_id.clone(), hydrated.generation);
    send_client_frame(
        &mut hydrated.socket,
        &socket_id,
        None,
        Some(claim(SESSION, generation)),
    )
    .await;
    let CoordWorkerDownstream::TerminalInputRouteClaim(sent_claim) =
        wait_for_sent(&hydrated.sent, |frame| {
            matches!(frame, CoordWorkerDownstream::TerminalInputRouteClaim(_))
        })
        .await
    else {
        unreachable!("filtered to a route claim");
    };
    let result = WTerminalInputRouteResult {
        request_id: sent_claim.request_id.clone(),
        result: roost_proto::buffa::MessageField::some(TerminalInputRouteResult {
            request_id: sent_claim.request_id.clone(),
            session_id: SESSION.to_owned(),
            revision: 1,
            accepted: true,
            latest_revision: 1,
            input_route_epoch: "route-epoch-e2e".to_owned(),
            worker_epoch: WORKER_EPOCH.to_owned(),
            ..Default::default()
        }),
        ..Default::default()
    };
    let owner = hydrated.fixture.services.terminal_input.route_results();
    assert!(owner.accept_input_route_result(&hydrated.worker, &result));

    let Frame::InputRouteResult(answer) = next_answer(&mut hydrated.socket).await else {
        panic!("expected an input route result");
    };
    assert!(answer.accepted);
    assert_eq!(answer.request_id, "claim-e2e");
    assert_eq!(answer.input_route_epoch, "route-epoch-e2e");

    drop(hydrated.socket);
    let CoordWorkerDownstream::TerminalViewSocketClosed(closed) =
        wait_for_sent(&hydrated.sent, |frame| {
            matches!(frame, CoordWorkerDownstream::TerminalViewSocketClosed(_))
        })
        .await
    else {
        unreachable!("filtered to a socket close");
    };
    assert_eq!(closed.socket_id, socket_id);
}
