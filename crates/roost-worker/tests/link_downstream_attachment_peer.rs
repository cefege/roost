//! The attachment-peer downstream arms with an attachment peer owner attached:
//! an offer's answer or typed refusal goes out fenced to the connection it
//! arrived on, a failed owner answers `ice_failed`, and a cancel reaches the
//! owner. Ports the peer half of v2
//! `apps/worker/tests/transport/coord-link-direct-terminal.test.ts`
//! ("attachment controls emit typed answer, error, and durable status").
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode, next_uplink, settle_tasks};
use roost_proto::{
    DLocalAttachmentPeerCancel, DLocalAttachmentPeerOffer, WLocalAttachmentPeerAnswer,
};
use roost_protocol::attachment_transfer::PeerErrorReason;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up,
};
use roost_worker::link_ports::AttachmentPeerPort;
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::{LinkFence, OwnerFuture, RequestBudget, UplinkReceiver, channel};

const EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const PEER_ID: &str = "11111111-1111-4111-8111-111111111113";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// v2's fake `onLocalAttachmentPeerOffer`: `attachment-bad` is refused with
/// `invalid_offer`, `attachment-panic` fails outright, the rest are answered.
#[derive(Debug, Default)]
struct FakeAttachmentPeers {
    calls: Mutex<Vec<String>>,
}

impl AttachmentPeerPort for FakeAttachmentPeers {
    fn offer(
        &self,
        request: DLocalAttachmentPeerOffer,
        _: RequestBudget,
        _: LinkFence,
    ) -> OwnerFuture<Result<WLocalAttachmentPeerAnswer, PeerErrorReason>> {
        lock(&self.calls).push(format!("offer:{}", request.request_id));
        Box::pin(async move {
            match request.request_id.as_str() {
                "attachment-bad" => Err(PeerErrorReason::InvalidOffer),
                "attachment-panic" => panic!("the fake attachment peer owner failed on purpose"),
                _ => Ok(WLocalAttachmentPeerAnswer {
                    request_id: request.request_id,
                    connection_generation: request.connection_generation,
                    worker_epoch: EPOCH.into(),
                    peer_id: request.peer_id,
                    answer_sdp: "answer".into(),
                    ..Default::default()
                }),
            }
        })
    }

    fn cancel(&self, request: &DLocalAttachmentPeerCancel) {
        lock(&self.calls).push(format!("cancel:{}", request.request_id));
    }
}

fn offer(request_id: &str) -> Down {
    Down::LocalAttachmentPeerOffer(DLocalAttachmentPeerOffer {
        request_id: request_id.into(),
        connection_generation: "attachment-generation".into(),
        worker_epoch: EPOCH.into(),
        grant_id: "attachment-grant".into(),
        peer_id: PEER_ID.into(),
        device_fingerprint: "a".repeat(64),
        tab_id: "attachment-tab".into(),
        offer_sdp: "offer".into(),
        budget_ms: 8_000,
        ..Default::default()
    })
}

fn dispatcher(peers: &Arc<FakeAttachmentPeers>) -> (Dispatcher, UplinkReceiver) {
    let (uplink, receiver) = channel();
    let mut owners = Fakes::new(OwnerMode::Answer).owners();
    owners.attachment_peers = Some(Arc::clone(peers) as Arc<dyn AttachmentPeerPort>);
    (Dispatcher::new(uplink, EPOCH, Some(owners)), receiver)
}

#[tokio::test]
async fn attachment_controls_emit_a_typed_answer_and_error_and_route_the_cancel() {
    let peers = Arc::new(FakeAttachmentPeers::default());
    let (dispatcher, mut receiver) = dispatcher(&peers);
    let mut link = FakeLink::default();
    dispatcher.dispatch(offer("attachment-ok"), Instant::now(), &mut link);
    dispatcher.dispatch(offer("attachment-bad"), Instant::now(), &mut link);
    let cancel = DLocalAttachmentPeerCancel {
        request_id: "attachment-ok".into(),
        connection_generation: "attachment-generation".into(),
        worker_epoch: EPOCH.into(),
        peer_id: PEER_ID.into(),
        ..Default::default()
    };
    dispatcher.dispatch(
        Down::LocalAttachmentPeerCancel(cancel),
        Instant::now(),
        &mut link,
    );

    let mut answered = Vec::new();
    for _ in 0..2 {
        match next_uplink(&mut receiver).await {
            Up::LocalAttachmentPeerAnswer(answer) => {
                answered.push(format!(
                    "answer:{}:{}",
                    answer.request_id, answer.answer_sdp
                ));
            }
            Up::LocalAttachmentPeerError(error) => {
                assert_eq!(
                    (
                        error.connection_generation.as_str(),
                        error.worker_epoch.as_str(),
                        error.peer_id.as_str()
                    ),
                    ("attachment-generation", EPOCH, PEER_ID)
                );
                answered.push(format!("error:{}:{}", error.request_id, error.reason));
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    answered.sort();
    assert_eq!(
        answered,
        [
            "answer:attachment-ok:answer",
            "error:attachment-bad:invalid_offer"
        ]
    );
    assert!(
        link.replies.is_empty(),
        "owner answers go through the fenced uplink"
    );
    assert_eq!(
        lock(&peers.calls).as_slice(),
        [
            "offer:attachment-ok",
            "offer:attachment-bad",
            "cancel:attachment-ok"
        ]
    );
}

#[tokio::test]
async fn a_failed_attachment_peer_owner_answers_ice_failed() {
    let peers = Arc::new(FakeAttachmentPeers::default());
    let (dispatcher, mut receiver) = dispatcher(&peers);
    let mut link = FakeLink::default();
    dispatcher.dispatch(offer("attachment-panic"), Instant::now(), &mut link);
    let Up::LocalAttachmentPeerError(error) = next_uplink(&mut receiver).await else {
        panic!("a peer error")
    };
    assert_eq!(
        (error.request_id.as_str(), error.reason.as_str()),
        ("attachment-panic", "ice_failed")
    );
}

#[tokio::test]
async fn an_attachment_answer_for_a_superseded_coordinator_connection_is_dropped() {
    let peers = Arc::new(FakeAttachmentPeers::default());
    let (dispatcher, mut receiver) = dispatcher(&peers);
    let mut link = FakeLink::default();
    receiver.advance();
    dispatcher.dispatch(offer("before"), Instant::now(), &mut link);
    receiver.advance();
    settle_tasks().await;
    assert!(
        receiver.try_recv().is_none(),
        "the answer was fenced to the connection that asked"
    );
}
