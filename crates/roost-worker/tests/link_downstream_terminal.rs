//! The downstream arms whose owners exist route to them with v2's semantics:
//! hello-ack order, the snapshot-ready edge, PTY-bound binary only, input and
//! stream requests answered once each (fenced) with v2's failure frame when the
//! owner panics or is dropped, the stream in-flight cap, synchronous view relay,
//! and route claims. Ports `coord-link-direct-terminal.test.ts` (route claim) and
//! `coord-link-terminal-pipeline.test.ts` ("dispatches the typed downstream
//! request directly to its owner") from `apps/worker/tests/transport/`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode, SESSION, next_uplink, settle_tasks};
use roost_proto::{DInputRequest, DTerminalInputRouteClaim, DTerminalPipelineSnapshotRequest};
use roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1;
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::{
    Binary, CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, DIR_FROM_PTY, DIR_TO_PTY,
    TerminalInputStatus, TerminalWritePhase,
};
use roost_worker::link_ports::LinkPipelineState;
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::{UplinkReceiver, channel};

const EPOCH: &str = "process-epoch-7";

fn dispatcher(fakes: &Fakes) -> (Dispatcher, UplinkReceiver) {
    let (uplink, receiver) = channel();
    (
        Dispatcher::new(uplink, EPOCH, Some(fakes.owners())),
        receiver,
    )
}

fn input_request(request_id: &str) -> Down {
    Down::InputRequest(DInputRequest {
        request_id: request_id.to_owned(),
        session_id: SESSION.to_owned(),
        input_seq: 7,
        data: b"ls\r".to_vec(),
        budget_ms: 5_000,
        ..Default::default()
    })
}

fn claim(request_id: &str) -> Down {
    Down::TerminalInputRouteClaim(DTerminalInputRouteClaim {
        request_id: request_id.to_owned(),
        session_id: SESSION.to_owned(),
        revision: 4,
        budget_ms: 8_000,
        ..Default::default()
    })
}

fn hello_ack(capabilities: Vec<String>) -> Down {
    Down::HelloAck {
        capabilities,
        trace_id: None,
    }
}

#[tokio::test]
async fn hello_ack_moves_the_barrier_then_drops_coordinator_sockets_then_tells_the_session() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (dispatcher, _receiver) = dispatcher(&fakes);
    let mut link = FakeLink {
        log: fakes.log.clone(),
        ..FakeLink::default()
    };
    dispatcher.dispatch(
        hello_ack(vec![CAPABILITY_TERMINAL_METADATA_V1.to_owned()]),
        Instant::now(),
        &mut link,
    );
    dispatcher.dispatch(
        hello_ack(vec!["terminal-view-owner-v1".to_owned()]),
        Instant::now(),
        &mut link,
    );
    assert_eq!(
        fakes.log.calls(),
        [
            "link.hello_acknowledged:true",
            "view.drop_coordinator_sockets",
            "lifecycle.on_hello_ack:true",
            "link.hello_acknowledged:false",
            "view.drop_coordinator_sockets",
            "lifecycle.on_hello_ack:false",
        ]
    );
}

#[tokio::test]
async fn only_the_ack_that_takes_the_link_live_is_snapshot_ready() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (dispatcher, _receiver) = dispatcher(&fakes);
    let mut link = FakeLink {
        log: fakes.log.clone(),
        ..FakeLink::default()
    };
    let ack =
        |seq| Down::EventAck(roost_protocol::wire::coord_worker::EventAck { client_seq: seq });
    dispatcher.dispatch(ack(1), Instant::now(), &mut link);
    link.goes_live = true;
    dispatcher.dispatch(ack(2), Instant::now(), &mut link);
    assert_eq!(
        fakes.log.calls(),
        [
            "link.event_acknowledged:1",
            "link.event_acknowledged:2",
            "lifecycle.on_snapshot_ready"
        ]
    );
}

#[tokio::test]
async fn only_pty_bound_binary_reaches_the_input_owner() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (dispatcher, _receiver) = dispatcher(&fakes);
    let mut link = FakeLink::default();
    let channel_id = ChannelId::try_from(3_i64).unwrap();
    for direction in [DIR_TO_PTY, DIR_FROM_PTY] {
        let binary = Binary {
            channel_id,
            direction,
            data: b"abc".to_vec(),
            seq: 0,
        };
        dispatcher.dispatch(Down::Binary(binary), Instant::now(), &mut link);
    }
    assert_eq!(fakes.log.calls(), ["input.write_binary:3:3"]);
    assert!(link.replies.is_empty());
}

#[tokio::test]
async fn an_input_request_is_answered_once_with_the_owners_result() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (dispatcher, mut receiver) = dispatcher(&fakes);
    let mut link = FakeLink::default();
    dispatcher.dispatch(input_request("in-1"), Instant::now(), &mut link);
    assert_eq!(
        fakes.log.calls(),
        ["input.write_input:in-1"],
        "the owner is called synchronously"
    );
    let Up::InputResult(result) = next_uplink(&mut receiver).await else {
        panic!("an input-result")
    };
    assert_eq!(
        (
            result.request_id.as_str(),
            result.status,
            result.written_bytes
        ),
        ("in-1", TerminalInputStatus::Accepted, 3)
    );
    settle_tasks().await;
    assert!(receiver.try_recv().is_none(), "exactly one answer");
    assert!(link.replies.is_empty());
}

#[tokio::test]
async fn a_panicking_input_owner_is_answered_ambiguous_with_an_unknown_phase() {
    let fakes = Fakes::new(OwnerMode::Panic);
    let (dispatcher, mut receiver) = dispatcher(&fakes);
    dispatcher.dispatch(
        input_request("in-2"),
        Instant::now(),
        &mut FakeLink::default(),
    );
    let Up::InputResult(result) = next_uplink(&mut receiver).await else {
        panic!("an input-result")
    };
    assert_eq!(result.status, TerminalInputStatus::Ambiguous);
    assert_eq!(result.phase, TerminalWritePhase::Unknown);
    assert_eq!(result.written_bytes, 0);
    assert_eq!(result.reason, "the fake owner failed on purpose");
}

#[test]
fn an_input_owner_dropped_before_answering_is_answered_ambiguous() {
    let fakes = Fakes::new(OwnerMode::Hold);
    let (dispatcher, mut receiver) = dispatcher(&fakes);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        dispatcher.dispatch(
            input_request("in-3"),
            Instant::now(),
            &mut FakeLink::default(),
        );
        settle_tasks().await;
    });
    assert!(receiver.try_recv().is_none(), "the owner is still holding");
    drop(runtime);
    let Some(Up::InputResult(result)) = receiver.try_recv() else {
        panic!("a cancelled owner still answers")
    };
    assert_eq!(
        (result.status, result.phase),
        (TerminalInputStatus::Ambiguous, TerminalWritePhase::Unknown)
    );
}

#[tokio::test]
async fn an_owner_reply_produced_after_a_redial_is_dropped() {
    let fakes = Fakes::new(OwnerMode::Hold);
    let (dispatcher, mut receiver) = dispatcher(&fakes);
    dispatcher.dispatch(
        input_request("in-4"),
        Instant::now(),
        &mut FakeLink::default(),
    );
    dispatcher.dispatch(claim("claim-4"), Instant::now(), &mut FakeLink::default());
    receiver.advance();
    fakes.gate.add_permits(2);
    settle_tasks().await;
    assert!(
        receiver.try_recv().is_none(),
        "a reply for the superseded connection never reaches the new one"
    );
    assert_eq!(fakes.log.count("input."), 2);
}

#[tokio::test]
async fn a_route_claim_is_the_owners_result_or_v2s_refused_claim() {
    let mut fakes = Fakes::new(OwnerMode::Answer);
    let (dispatcher, mut receiver) = dispatcher(&fakes);
    dispatcher.dispatch(claim("claim-1"), Instant::now(), &mut FakeLink::default());
    let Up::TerminalInputRouteResult(accepted) = next_uplink(&mut receiver).await else {
        panic!("a route result")
    };
    assert_eq!(accepted.request_id, "claim-1");
    assert!(accepted.result.accepted);

    fakes.claim_accepts = false;
    let (busy, mut busy_receiver) = self::dispatcher(&fakes);
    busy.dispatch(claim("claim-2"), Instant::now(), &mut FakeLink::default());
    let Up::TerminalInputRouteResult(refused) = next_uplink(&mut busy_receiver).await else {
        panic!("a route result")
    };
    assert_eq!(refused.request_id, "claim-2");
    let result = &*refused.result;
    assert_eq!(
        (
            result.request_id.as_str(),
            result.session_id.as_str(),
            result.revision,
            result.accepted
        ),
        ("claim-2", SESSION, 4, false)
    );
    assert_eq!(
        (result.latest_revision, result.input_route_epoch.as_str()),
        (0, "")
    );
    assert_eq!(
        (result.worker_epoch.as_str(), result.reason.as_str()),
        (EPOCH, "route_claim_busy")
    );
}

#[tokio::test]
async fn pipeline_snapshot_is_answered_synchronously_with_the_links_queue_state() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (dispatcher, _receiver) = dispatcher(&fakes);
    let state = LinkPipelineState {
        queue_frames: 5,
        queue_bytes: 9,
        native_buffered_bytes: 0,
        attached: true,
    };
    let mut link = FakeLink {
        state,
        ..FakeLink::default()
    };
    let request = DTerminalPipelineSnapshotRequest {
        request_id: "pipe-1".to_owned(),
        ..Default::default()
    };
    dispatcher.dispatch(
        Down::TerminalPipelineSnapshot(request),
        Instant::now(),
        &mut link,
    );
    assert_eq!(fakes.log.calls(), ["pipeline.snapshot:pipe-1:5"]);
    let [Up::TerminalPipelineSnapshot(snapshot)] = link.replies.as_slice() else {
        panic!("one snapshot")
    };
    assert_eq!(
        (snapshot.request_id.as_str(), snapshot.dropped_targets),
        ("pipe-1", 9)
    );
}

#[tokio::test]
async fn view_relay_snapshot_repair_and_socket_close_route_in_receive_order() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (dispatcher, _receiver) = dispatcher(&fakes);
    let mut link = FakeLink::default();
    let relay = |socket: &str| {
        Down::TerminalViewRelay(roost_proto::DTerminalViewRelay {
            socket_id: socket.to_owned(),
            ..Default::default()
        })
    };
    dispatcher.dispatch(relay("s1"), Instant::now(), &mut link);
    dispatcher.dispatch(
        Down::TerminalSnapshotRequest(
            roost_protocol::wire::coord_worker::TerminalSnapshotRequest {
                session_id: SESSION.try_into().unwrap(),
                stream_id: "stream-1".to_owned(),
            },
        ),
        Instant::now(),
        &mut link,
    );
    dispatcher.dispatch(relay("s2"), Instant::now(), &mut link);
    dispatcher.dispatch(
        Down::TerminalViewSocketClosed(roost_proto::DTerminalViewSocketClosed {
            socket_id: "s1".to_owned(),
            ..Default::default()
        }),
        Instant::now(),
        &mut link,
    );
    assert_eq!(
        fakes.log.calls(),
        [
            "view.relay:s1",
            "stream.request_snapshot:stream-1",
            "view.relay:s2",
            "input.retire_connection:s1",
            "view.close_socket:s1",
        ]
    );
    assert!(link.replies.is_empty());
}
