//! The downstream arms whose owner lands in a later wave answer exactly what a
//! v2 worker without that `CoordLinkDeps` callback answers, the link-level arms
//! reach the link, and a worker with no owners at all refuses input the way
//! v2 does. Ports the absent-dependency branches of
//! `apps/worker/src/transport/coord-link-downstream.ts` and
//! `coord-link-direct-terminal.ts`, and the POSIX update-broker answer.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode, SESSION, settle_tasks};
use roost_proto::{
    DAgentPrompt, DAttachmentChunk, DAttachmentDirectStatusRequest, DCoordMovePrepare,
    DCoordMoveSnapshotChunk, DCoordMoveSnapshotStart, DCoordRelocate, DInputRequest,
    DKeeperUpdatePrepare, DLocalAttachmentGrant, DLocalAttachmentGrantRevoke,
    DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer, DLocalTerminalGrant,
    DLocalTerminalGrantRevoke, DLocalTerminalPeerCancel, DLocalTerminalPeerOffer,
    DTerminalDirectRetire, DTerminalInputRouteClaim, DTerminalTransportProbe, DUpdateBroker,
};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, TerminalInputStatus,
    TerminalWritePhase,
};
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::channel;

const EPOCH: &str = "process-epoch-9";

/// Dispatch one frame to a worker WITH the wave-1 owners and return its
/// synchronous answers; no later-wave arm may touch a wave-1 owner.
fn answers(frame: Down) -> Vec<Up> {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, EPOCH, Some(fakes.owners()));
    let mut link = FakeLink::default();
    dispatcher.dispatch(frame, Instant::now(), &mut link);
    assert!(fakes.log.calls().is_empty(), "no wave-1 owner is involved");
    assert!(receiver.try_recv().is_none(), "nothing is answered later");
    link.replies
}

fn rpc_error(request_id: &str, message: &str) -> Vec<Up> {
    vec![Up::RpcError {
        request_id: request_id.to_owned(),
        message: message.to_owned(),
        trace_id: None,
    }]
}

/// The single answer a frame produced.
fn only(frames: Vec<Up>) -> Up {
    let [frame] = <[Up; 1]>::try_from(frames).unwrap();
    frame
}

#[test]
fn unsupported_grants_and_keeper_update_answer_v2s_rpc_errors() {
    let grant = DLocalTerminalGrant {
        request_id: "g1".to_owned(),
        ..Default::default()
    };
    assert_eq!(
        answers(Down::LocalTerminalGrant(grant)),
        rpc_error("g1", "local terminal grants unsupported by this worker")
    );
    let grant = DLocalAttachmentGrant {
        request_id: "g2".to_owned(),
        ..Default::default()
    };
    assert_eq!(
        answers(Down::LocalAttachmentGrant(grant)),
        rpc_error("g2", "local attachment grants unsupported by this worker")
    );
    let prepare = DKeeperUpdatePrepare {
        request_id: "k1".to_owned(),
        ..Default::default()
    };
    assert_eq!(
        answers(Down::KeeperUpdatePrepare(prepare)),
        rpc_error("k1", "keeper update preparation unsupported by this worker")
    );
}

/// v2's running POSIX worker: an undefined action is refused by the dispatch
/// (`coord-link-downstream.ts:342-344`); a defined one reaches `onUpdateBroker`,
/// whose darwin/linux arm throws (`coord-link-deps.ts:379-381`), and the
/// `.catch` answers the thrown message (`coord-link-downstream.ts:362-364`).
#[test]
fn update_broker_answers_as_v2s_posix_worker() {
    let broker = |action: &str| DUpdateBroker {
        request_id: "u1".to_owned(),
        action: action.to_owned(),
        ..Default::default()
    };
    assert_eq!(
        answers(Down::UpdateBroker(broker("REBOOT"))),
        rpc_error("u1", "unsupported updater action: REBOOT")
    );
    assert_eq!(
        answers(Down::UpdateBroker(broker(""))),
        rpc_error("u1", "unsupported updater action: ")
    );
    for action in ["START", "STATUS"] {
        assert_eq!(
            answers(Down::UpdateBroker(broker(action))),
            rpc_error(
                "u1",
                "Windows update broker command received on a POSIX worker"
            )
        );
    }
}

#[test]
fn an_agent_prompt_is_rejected_pre_write() {
    let prompt = DAgentPrompt {
        request_id: "p1".to_owned(),
        session_id: SESSION.to_owned(),
        input_seq: 11,
        text: "hi".to_owned(),
        ..Default::default()
    };
    let Up::InputResult(result) = only(answers(Down::AgentPrompt(prompt))) else {
        panic!("one input-result")
    };
    assert_eq!(
        (
            result.request_id.as_str(),
            result.session_id.as_str(),
            result.input_seq
        ),
        ("p1", SESSION, 11)
    );
    assert_eq!(
        (result.status, result.phase, result.written_bytes),
        (
            TerminalInputStatus::Rejected,
            TerminalWritePhase::PreWrite,
            0
        )
    );
    assert_eq!(result.reason, "worker agent prompt handler is unavailable");
}

#[test]
fn peer_offers_are_disabled_and_an_unknown_upload_is_not_found() {
    let offer = DLocalTerminalPeerOffer {
        request_id: "o1".to_owned(),
        connection_generation: "gen".to_owned(),
        peer_id: "peer".to_owned(),
        ..Default::default()
    };
    let Up::LocalTerminalPeerError(error) = only(answers(Down::LocalTerminalPeerOffer(offer)))
    else {
        panic!("a peer error")
    };
    assert_eq!(
        (
            error.request_id.as_str(),
            error.connection_generation.as_str(),
            error.peer_id.as_str()
        ),
        ("o1", "gen", "peer")
    );
    assert_eq!(
        (error.worker_epoch.as_str(), error.reason.as_str()),
        (EPOCH, "disabled")
    );

    let offer = DLocalAttachmentPeerOffer {
        request_id: "o2".to_owned(),
        connection_generation: "gen2".to_owned(),
        peer_id: "peer2".to_owned(),
        ..Default::default()
    };
    let Up::LocalAttachmentPeerError(error) = only(answers(Down::LocalAttachmentPeerOffer(offer)))
    else {
        panic!("a peer error")
    };
    assert_eq!(
        (
            error.request_id.as_str(),
            error.connection_generation.as_str(),
            error.peer_id.as_str()
        ),
        ("o2", "gen2", "peer2")
    );
    assert_eq!(
        (error.worker_epoch.as_str(), error.reason.as_str()),
        (EPOCH, "disabled")
    );

    let status = DAttachmentDirectStatusRequest {
        request_id: "s1".to_owned(),
        upload_id: "up-1".to_owned(),
        ..Default::default()
    };
    let Up::AttachmentDirectStatus(answer) =
        only(answers(Down::AttachmentDirectStatusRequest(status)))
    else {
        panic!("a status")
    };
    assert_eq!(answer.request_id, "s1");
    let status = answer.status.as_option().expect("the status is set");
    assert_eq!(
        (status.upload_id.as_str(), status.error.as_str()),
        ("up-1", "upload_not_found")
    );
    assert_eq!(
        (status.next_seq, status.bytes_received, status.committed),
        (0, 0, false)
    );
    assert_eq!(
        (status.last_chunk_sha256.as_str(), status.abs_path.as_str()),
        ("", "")
    );
}

#[test]
fn optional_callbacks_and_retired_tags_are_inert() {
    let inert = [
        Down::TerminalTransportProbe(DTerminalTransportProbe::default()),
        Down::LocalTerminalGrantRevoke(DLocalTerminalGrantRevoke::default()),
        Down::LocalAttachmentGrantRevoke(DLocalAttachmentGrantRevoke::default()),
        Down::LocalTerminalPeerCancel(DLocalTerminalPeerCancel::default()),
        Down::LocalAttachmentPeerCancel(DLocalAttachmentPeerCancel::default()),
        Down::TerminalDirectRetire(DTerminalDirectRetire::default()),
        Down::AttachmentChunk(DAttachmentChunk::default()),
        Down::CoordMovePrepare(DCoordMovePrepare::default()),
        Down::CoordMoveSnapshotStart(DCoordMoveSnapshotStart::default()),
        Down::CoordMoveSnapshotChunk(DCoordMoveSnapshotChunk::default()),
        Down::CoordRelocate(DCoordRelocate::default()),
    ];
    for frame in inert {
        let kind = frame.kind();
        assert!(answers(frame).is_empty(), "{kind} is answered by nobody");
    }
}

#[tokio::test]
async fn ping_is_ponged_and_a_browser_command_goes_to_the_pump_fenced_to_this_connection() {
    let (uplink, _receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, EPOCH, None);
    let mut link = FakeLink::default();
    dispatcher.dispatch(
        Down::Ping {
            ts: 42,
            trace_id: None,
        },
        Instant::now(),
        &mut link,
    );
    assert_eq!(
        link.replies,
        [Up::Pong {
            ts: 42,
            trace_id: None
        }]
    );
    let frame = roost_protocol::wire::control::ClientControlFrame::parse(serde_json::json!({
        "kind": "attach", "session_id": SESSION
    }))
    .expect("a canonical attach");
    let command = Down::BrowserCommand {
        browser_id: "b".to_owned(),
        viewer_id: "v".to_owned(),
        request_id: "outer".to_owned(),
        frame,
        trace_id: None,
    };
    dispatcher.dispatch(command, Instant::now(), &mut link);
    let [(command, fence)] = link.commands.as_slice() else {
        panic!("one command handed to the pump")
    };
    assert_eq!(command.request_id, "outer");
    assert!(fence.is_current());
}

#[tokio::test]
async fn a_worker_without_owners_refuses_input_and_claims_as_v2_does() {
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, EPOCH, None);
    let mut link = FakeLink::default();
    let input = DInputRequest {
        request_id: "i1".to_owned(),
        session_id: SESSION.to_owned(),
        input_seq: 2,
        ..Default::default()
    };
    dispatcher.dispatch(Down::InputRequest(input), Instant::now(), &mut link);
    let claim = DTerminalInputRouteClaim {
        request_id: "c1".to_owned(),
        session_id: SESSION.to_owned(),
        revision: 5,
        ..Default::default()
    };
    dispatcher.dispatch(
        Down::TerminalInputRouteClaim(claim),
        Instant::now(),
        &mut link,
    );
    settle_tasks().await;
    assert!(receiver.try_recv().is_none());
    let [Up::InputResult(result), Up::TerminalInputRouteResult(route)] = link.replies.as_slice()
    else {
        panic!("{:?}", link.replies)
    };
    assert_eq!(
        (result.status, result.phase),
        (TerminalInputStatus::Rejected, TerminalWritePhase::PreWrite)
    );
    assert_eq!(result.reason, "worker input handler is unavailable");
    assert_eq!(
        (route.result.accepted, route.result.reason.as_str()),
        (false, "route_claim_busy")
    );
    assert_eq!(route.result.worker_epoch, EPOCH);
}
