//! The worker link's completion lane over a real socket: while a durable append
//! is parked, a typed input result still settles its waiter, and every other
//! frame keeps its place behind the append.
//!
//! Ports the `inputResult` bypass of `apps/coord/src/workers/worker-ws-handler.ts:235-254`
//! as `apps/coord/tests/workers/worker-ws-result-dispatch.test.ts` "settles
//! input and stream results without waiting for a durable handler" drives it.
//! The append is parked by holding the coordinator's one pooled database
//! connection. `unwrap`/`expect` are denied outside `#[cfg(test)]`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod worker_link_wire_support;
mod ws_client_support;
mod ws_credential_support;

use std::time::Duration;

use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream, CoordWorkerUpstream, EventAck, InputResult, TerminalInputStatus,
    TerminalWritePhase,
};
use roost_protocol::wire::{SessionEvent, SessionId};
use serde_json::json;
use worker_link_wire_support::{WireFixture, next_downstream, upstream_bytes};
use ws_client_support::send_binary;

fn snapshot(fixture: &WireFixture, client_seq: u64) -> Vec<u8> {
    upstream_bytes(&CoordWorkerUpstream::Event {
        event: SessionEvent::Snapshot {
            worker_fp: fixture.fp(),
            sessions: Vec::new(),
            ts: 1_500,
            trace_id: None,
        },
        client_seq,
        trace_id: None,
    })
}

fn input_result(request_id: &str) -> Vec<u8> {
    upstream_bytes(&CoordWorkerUpstream::InputResult(InputResult {
        request_id: request_id.to_owned(),
        session_id: SessionId::try_from("11111111-1111-4111-8111-111111111111").unwrap(),
        input_seq: 1,
        status: TerminalInputStatus::Accepted,
        written_bytes: 1,
        reason: String::new(),
        phase: TerminalWritePhase::Written,
    }))
}

/// The next frame that is not a keepalive ping.
async fn next_non_ping(socket: &mut ws_client_support::WsClient) -> CoordWorkerDownstream {
    loop {
        match next_downstream(socket).await.expect("a coordinator frame") {
            CoordWorkerDownstream::Ping { .. } => {}
            frame => return frame,
        }
    }
}

#[tokio::test]
async fn an_input_result_overtakes_a_parked_append_and_an_rpc_reply_waits_behind_it() {
    let fixture = WireFixture::start("result-lane").await;
    let (mut socket, _ack) = fixture.hello_link().await;
    send_binary(&mut socket, snapshot(&fixture, 1)).await;
    assert_eq!(
        next_non_ping(&mut socket).await,
        CoordWorkerDownstream::EventAck(EventAck { client_seq: 1 })
    );
    let table = fixture.services.scrollback.pending();
    let mut input = table.create_fresh(Some(&fixture.worker_fp), 0).unwrap();
    let mut ordered = table.create_fresh(Some(&fixture.worker_fp), 0).unwrap();

    let held = fixture.services.db.pool().acquire().await.unwrap();
    send_binary(&mut socket, snapshot(&fixture, 2)).await;
    send_binary(&mut socket, input_result(input.request_id())).await;
    let rpc_ok = CoordWorkerUpstream::RpcOk {
        request_id: ordered.request_id().to_owned(),
        data: json!({ "ordered": true }),
        trace_id: None,
    };
    send_binary(&mut socket, upstream_bytes(&rpc_ok)).await;

    let settled = tokio::time::timeout(Duration::from_secs(2), input.settle_typed::<InputResult>())
        .await
        .expect("the input result settles while the append is still parked")
        .unwrap();
    assert_eq!(settled.request_id, input.request_id());
    assert!(
        tokio::time::timeout(Duration::from_millis(300), ordered.settle())
            .await
            .is_err(),
        "an rpc reply keeps its place behind the durable append"
    );

    drop(held);
    assert_eq!(
        next_non_ping(&mut socket).await,
        CoordWorkerDownstream::EventAck(EventAck { client_seq: 2 })
    );
    let reply = tokio::time::timeout(Duration::from_secs(2), ordered.settle())
        .await
        .expect("the backlog drains once the append settles")
        .unwrap();
    assert_eq!(reply, json!({ "ordered": true }));
}
