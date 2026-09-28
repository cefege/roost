//! One input batch through its sender lane to exactly one keeper write: the
//! monotonic hop budget, phase-proven rejection, lane order, stale-route
//! refusal, Sync-generation cancellation, and the unary `SessionsInput` answer.
//! Ports the input cases of `apps/coord/tests/terminal/terminal-hop-deadline.test.ts`
//! and "input cannot repopulate a stale route" of `durable-publication-snapshot.test.ts`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod terminal_input_support;

use std::sync::Arc;

use roost_coord::auth::principal::Principal;
use roost_coord::coord_core::CoordCore;
use roost_coord::coord_core::caller::{Caller, ListenerTrust};
use roost_coord::coord_core::seams::{LiveChannel, WorkerRouteIndex};
use roost_coord::terminal_input::input_control::{InputRouteAuthority, process_input_control};
use roost_coord::terminal_input::rpc_input::handle_sessions_input;
use roost_coord::terminal_input::write_control::TerminalWriteStatus;
use roost_coord::workers::hop_deadline::{HopDeadline, INPUT_CONTROL_TIMEOUT_MS};
use roost_proto::SessionsInputRequest;
use roost_protocol::wire::coord_worker::{InputResult, TerminalInputStatus, TerminalWritePhase};
use roost_protocol::wire::{ChannelId, SessionId, WorkerFp};
use terminal_input_support::{
    CALLER_FP, InputHarness, WORKER_FP, batch, identity, written_in_full,
};

// v2: "a budget too short to survive the hop is refused rather than half-spent"
#[tokio::test]
async fn a_budget_too_short_to_survive_the_hop_is_refused_before_send() {
    let harness = InputHarness::new("presend").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(written_in_full));
    let mut command = batch("tab-input-presend", &session, 1, b"a");
    command.deadline = Some(HopDeadline::start(900));

    let result = process_input_control(&harness.services, command).await;

    assert_eq!(result.status, TerminalWriteStatus::Rejected);
    assert_eq!(result.reason, "input budget expired before worker send");
    assert_eq!(result.written_bytes, 0);
    assert!(harness.input_requests().is_empty());
}

// v2: "an expired budget stays expired no matter what the wall clock says"
#[tokio::test]
async fn an_expired_budget_is_refused_before_send() {
    let harness = InputHarness::new("expired").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(written_in_full));
    let mut command = batch("tab-skew-expired", &session, 1, b"a");
    command.deadline = Some(HopDeadline::start(0));

    let result = process_input_control(&harness.services, command).await;

    assert_eq!(result.status, TerminalWriteStatus::Rejected);
    assert_eq!(result.reason, "input budget expired before worker send");
    assert!(harness.input_requests().is_empty());
}

// v2: "a healthy remaining budget still sends, and the worker slice is strictly smaller"
#[tokio::test]
async fn a_healthy_budget_sends_a_strictly_smaller_worker_slice_and_the_actor() {
    let harness = InputHarness::new("nested").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(written_in_full));
    let mut command = batch("tab-input-nested", &session, 1, b"a");
    command.deadline = Some(HopDeadline::start(4_000));
    command.input_route_authority = Some(InputRouteAuthority {
        device_fingerprint: CALLER_FP.to_owned(),
        tab_id: "tab-input-nested".to_owned(),
        connection_id: "sync-input-nested".to_owned(),
        input_route_epoch: "route-epoch-nested".to_owned(),
    });

    let result = process_input_control(&harness.services, command).await;

    assert_eq!(result.status, TerminalWriteStatus::Accepted);
    let requests = harness.input_requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].budget_ms > 0 && requests[0].budget_ms < 4_000);
    assert_eq!(requests[0].device_fingerprint, CALLER_FP);
    assert_eq!(requests[0].tab_id, "tab-input-nested");
    assert_eq!(requests[0].browser_connection_id, "sync-input-nested");
    assert_eq!(requests[0].input_route_epoch, "route-epoch-nested");
}

// v2: "input REJECTED without a pre-write phase is ambiguous, so it is never retried"
#[tokio::test]
async fn a_rejection_without_pre_write_proof_is_ambiguous_and_never_retried() {
    let harness = InputHarness::new("unproven").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(|request| {
        Some(InputResult {
            request_id: request.request_id.clone(),
            session_id: SessionId::try_from(request.session_id.as_str()).unwrap(),
            input_seq: request.input_seq,
            status: TerminalInputStatus::Rejected,
            written_bytes: 0,
            reason: "keeper write outcome unproven".to_owned(),
            phase: TerminalWritePhase::Written,
        })
    }));

    let result = process_input_control(
        &harness.services,
        batch("tab-input-unproven", &session, 1, b"ls"),
    )
    .await;

    assert_eq!(result.status, TerminalWriteStatus::Ambiguous);
    assert_eq!(harness.input_requests().len(), 1);
}

// v2 terminal-write-control.ts workerResultAccepted: raw input is accepted only
// when the keeper wrote EXACTLY the batch; a short write is ambiguous.
#[tokio::test]
async fn an_acceptance_that_wrote_fewer_bytes_than_sent_is_ambiguous() {
    let harness = InputHarness::new("short-write").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(|request| {
        let mut result = written_in_full(request)?;
        result.written_bytes -= 1;
        Some(result)
    }));

    let result =
        process_input_control(&harness.services, batch("tab-short", &session, 1, b"ls")).await;

    assert_eq!(result.status, TerminalWriteStatus::Ambiguous);
    assert_eq!(result.written_bytes, 1);
}

// v2: "caller buffer mutation cannot change a same-lane queued request"
#[tokio::test]
async fn same_lane_batches_reach_the_worker_in_the_order_they_entered() {
    let harness = InputHarness::new("owned").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(written_in_full));

    let first = process_input_control(
        &harness.services,
        batch("tab-owned-input", &session, 1, b"a"),
    );
    let second = process_input_control(
        &harness.services,
        batch("tab-owned-input", &session, 2, b"bc"),
    );
    // The later batch is polled first, so only the lane can restore the order.
    let (later, earlier) = tokio::join!(second, first);

    assert_eq!(
        (earlier.status, earlier.written_bytes),
        (TerminalWriteStatus::Accepted, 1)
    );
    assert_eq!(
        (later.status, later.written_bytes),
        (TerminalWriteStatus::Accepted, 2)
    );
    let payloads: Vec<Vec<u8>> = harness
        .input_requests()
        .into_iter()
        .map(|request| request.data)
        .collect();
    assert_eq!(payloads, vec![b"a".to_vec(), b"bc".to_vec()]);
}

// v2 (durable-publication-snapshot.test.ts): "input cannot repopulate a stale route from the open breadcrumb"
#[tokio::test]
async fn input_cannot_repopulate_a_stale_route_from_the_open_breadcrumb() {
    let harness = InputHarness::new("stale-route").await;
    let live = harness.seed_session(1, 21).await;
    let stale = harness.seed_session(2, 12).await;
    let hub = &harness.services.byte_hub;
    let stale_id = SessionId::try_from(stale.as_str()).unwrap();

    let before = process_input_control(&harness.services, batch("tab-1", &stale, 1, b"ls\r")).await;
    assert_eq!(before.status, TerminalWriteStatus::Rejected);
    assert_eq!(
        before.reason, "worker unavailable",
        "pre-reconcile, the breadcrumb routes"
    );

    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    let live_set = [LiveChannel {
        session_id: SessionId::try_from(live.as_str()).unwrap(),
        channel_id: ChannelId::try_from(21_i64).unwrap(),
    }];
    WorkerRouteIndex::replace_worker_channel_index(hub.as_ref(), &worker, &live_set);

    let after = process_input_control(&harness.services, batch("tab-1", &stale, 2, b"ls\r")).await;
    assert_eq!(after.status, TerminalWriteStatus::Rejected);
    assert_eq!(after.reason, "unknown session");
    assert_eq!(hub.cached_route(&stale_id), None);
}

// v2 (terminal-control-lane.ts cancelTerminalControlGeneration): a closing Sync
// generation cancels the queued batches that have not begun.
#[tokio::test]
async fn a_closed_sync_generation_cancels_its_input_that_has_not_begun() {
    let harness = InputHarness::new("generation").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(written_in_full));
    let held = harness.services.db.pool().acquire().await.unwrap();
    let on_socket = |seq| {
        let mut command = batch("tab-generation", &session, seq, b"x");
        command.socket_generation = Some("socket-1".to_owned());
        tokio::spawn(process_input_control(&harness.services, command))
    };
    let running = on_socket(1);
    let queued = on_socket(2);
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }

    let viewer_key = identity("tab-generation").viewer_key;
    harness
        .services
        .terminal_input
        .lanes()
        .cancel_generation(&viewer_key, "socket-1");
    drop(held);

    assert_eq!(running.await.unwrap().status, TerminalWriteStatus::Accepted);
    let cancelled = queued.await.unwrap();
    assert_eq!(cancelled.status, TerminalWriteStatus::Rejected);
    assert_eq!(cancelled.reason, "generation closed or control queue full");
    assert_eq!(harness.input_requests().len(), 1);
}

// v2 (handlers-sessions.ts sessionsInput): unary input is accepted only when
// the keeper proved the write, and a guessed session is simply not accepted.
#[tokio::test]
async fn sessions_input_is_accepted_only_for_a_proven_write() {
    let harness = InputHarness::new("unary").await;
    let session = harness.seed_session(1, 1).await;
    harness.attach_worker(Arc::new(written_in_full));
    let core = CoordCore::new(Arc::clone(&harness.services));
    let caller = Caller {
        principal: Principal::AccountDevice {
            fingerprint: CALLER_FP.to_owned(),
            label: "laptop".to_owned(),
            account_id: "account-1".to_owned(),
        },
        tab_id: Some("tab-unary".to_owned()),
        remote_address: None,
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    };
    let input = |session_id: &str| SessionsInputRequest {
        session_id: session_id.to_owned(),
        data: b"pwd\r".to_vec(),
        ..Default::default()
    };

    let written = handle_sessions_input(&core, &caller, input(&session))
        .await
        .unwrap();
    let guessed = handle_sessions_input(
        &core,
        &caller,
        input("00000000-0000-4000-8000-00000000abcd"),
    )
    .await
    .unwrap();

    assert!(written.body.accepted);
    assert!(!guessed.body.accepted);
    let requests = harness.input_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].input_seq, 1,
        "a unary batch carries a coordinator sequence"
    );
    assert!(
        requests[0].device_fingerprint.is_empty(),
        "unary input carries no route actor"
    );
    assert!(requests[0].budget_ms < u32::try_from(INPUT_CONTROL_TIMEOUT_MS).unwrap());
}
