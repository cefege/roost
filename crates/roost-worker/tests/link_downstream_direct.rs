//! The direct-terminal downstream arms with a direct owner attached: a peer
//! offer's answer or typed refusal goes out on the connection it arrived on
//! and never on a re-dialled one, a failed owner answers `ice_failed`, the
//! probe answers in receive order, and cancel and retire reach the owner.
//! Ports the terminal half of v2
//! `apps/worker/tests/transport/coord-link-direct-terminal.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode, next_uplink, settle_tasks};
use roost_proto::{
    DLocalTerminalPeerCancel, DLocalTerminalPeerOffer, DTerminalDirectRetire,
    DTerminalTransportProbe, WLocalTerminalPeerAnswer, WTerminalTransportProbeResult,
};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up,
};
use roost_worker::link_ports::DirectTerminalPort;
use roost_worker::peer::TerminalPeerOfferFailure;
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::{LinkFence, OwnerFuture, RequestBudget, channel};

const EPOCH: &str = "11111111-1111-4111-8111-111111111111";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How the fake owner settles an offer.
#[derive(Debug, Clone, Copy)]
enum Offer {
    Answer,
    Refuse(TerminalPeerOfferFailure),
    Panic,
}

#[derive(Debug)]
struct FakeDirect {
    offer: Offer,
    calls: Mutex<Vec<String>>,
}

impl FakeDirect {
    fn new(offer: Offer) -> Arc<Self> {
        Arc::new(Self {
            offer,
            calls: Mutex::new(Vec::new()),
        })
    }
    fn calls(&self) -> Vec<String> {
        lock(&self.calls).clone()
    }
}

impl DirectTerminalPort for FakeDirect {
    fn peer_offer(
        &self,
        request: DLocalTerminalPeerOffer,
        _: RequestBudget,
        _: LinkFence,
    ) -> OwnerFuture<Result<WLocalTerminalPeerAnswer, TerminalPeerOfferFailure>> {
        lock(&self.calls).push(format!("offer:{}", request.request_id));
        let offer = self.offer;
        Box::pin(async move {
            match offer {
                Offer::Answer => Ok(WLocalTerminalPeerAnswer {
                    request_id: request.request_id,
                    connection_generation: request.connection_generation,
                    worker_epoch: EPOCH.into(),
                    peer_id: request.peer_id,
                    answer_sdp: "answer".into(),
                    ..Default::default()
                }),
                Offer::Refuse(failure) => Err(failure),
                Offer::Panic => panic!("the fake direct owner failed on purpose"),
            }
        })
    }
    fn peer_cancel(&self, request: &DLocalTerminalPeerCancel) {
        lock(&self.calls).push(format!("cancel:{}", request.request_id));
    }
    fn transport_probe(
        &self,
        request: &DTerminalTransportProbe,
    ) -> Option<WTerminalTransportProbeResult> {
        (request.worker_epoch == EPOCH).then(|| WTerminalTransportProbeResult {
            request_id: request.request_id.clone(),
            worker_epoch: EPOCH.into(),
            ..Default::default()
        })
    }
    fn direct_retire(&self, request: &DTerminalDirectRetire) {
        lock(&self.calls).push(format!("retire:{}", request.reason));
    }
}

fn offer_frame(request_id: &str) -> Down {
    Down::LocalTerminalPeerOffer(DLocalTerminalPeerOffer {
        request_id: request_id.into(),
        connection_generation: "coord-generation".into(),
        worker_epoch: EPOCH.into(),
        peer_id: "11111111-1111-4111-8111-111111111112".into(),
        budget_ms: 8_000,
        ..Default::default()
    })
}

fn dispatcher(direct: &Arc<FakeDirect>) -> (Dispatcher, roost_worker::uplink::UplinkReceiver) {
    let (uplink, receiver) = channel();
    let mut owners = Fakes::new(OwnerMode::Answer).owners();
    owners.direct = Some(Arc::clone(direct) as Arc<dyn DirectTerminalPort>);
    (Dispatcher::new(uplink, EPOCH, Some(owners)), receiver)
}

#[tokio::test]
async fn direct_downstream_emits_typed_peer_and_probe_results_while_routing_cancel_and_retirement()
{
    let direct = FakeDirect::new(Offer::Answer);
    let (dispatcher, mut receiver) = dispatcher(&direct);
    let mut link = FakeLink::default();
    dispatcher.dispatch(offer_frame("peer-offer"), Instant::now(), &mut link);
    let probe = DTerminalTransportProbe {
        request_id: "probe".into(),
        worker_epoch: EPOCH.into(),
        ..Default::default()
    };
    dispatcher.dispatch(
        Down::TerminalTransportProbe(probe),
        Instant::now(),
        &mut link,
    );
    let cancel = DLocalTerminalPeerCancel {
        request_id: "peer-offer".into(),
        connection_generation: "coord-generation".into(),
        ..Default::default()
    };
    dispatcher.dispatch(
        Down::LocalTerminalPeerCancel(cancel),
        Instant::now(),
        &mut link,
    );
    let retire = DTerminalDirectRetire {
        worker_epoch: EPOCH.into(),
        reason: "worker_deleted".into(),
        ..Default::default()
    };
    dispatcher.dispatch(
        Down::TerminalDirectRetire(retire),
        Instant::now(),
        &mut link,
    );

    let Up::LocalTerminalPeerAnswer(answer) = next_uplink(&mut receiver).await else {
        panic!("a peer answer")
    };
    assert_eq!(
        (answer.request_id.as_str(), answer.answer_sdp.as_str()),
        ("peer-offer", "answer")
    );
    let [Up::TerminalTransportProbeResult(result)] = link.replies.as_slice() else {
        panic!("{:?}", link.replies)
    };
    assert_eq!(
        (result.request_id.as_str(), result.worker_epoch.as_str()),
        ("probe", EPOCH)
    );
    assert_eq!(
        direct.calls(),
        [
            "offer:peer-offer",
            "cancel:peer-offer",
            "retire:worker_deleted"
        ]
    );

    let stale = DTerminalTransportProbe {
        request_id: "stale".into(),
        worker_epoch: "another".into(),
        ..Default::default()
    };
    dispatcher.dispatch(
        Down::TerminalTransportProbe(stale),
        Instant::now(),
        &mut link,
    );
    assert_eq!(
        link.replies.len(),
        1,
        "a probe for another epoch is not answered"
    );
}

#[tokio::test]
async fn a_refused_offer_answers_its_reason_and_a_failed_owner_answers_ice_failed() {
    for (offer, reason) in [
        (
            Offer::Refuse(TerminalPeerOfferFailure::Capacity),
            "capacity",
        ),
        (Offer::Refuse(TerminalPeerOfferFailure::Expired), "expired"),
        (Offer::Panic, "ice_failed"),
    ] {
        let direct = FakeDirect::new(offer);
        let (dispatcher, mut receiver) = dispatcher(&direct);
        let mut link = FakeLink::default();
        dispatcher.dispatch(offer_frame("o1"), Instant::now(), &mut link);
        let Up::LocalTerminalPeerError(error) = next_uplink(&mut receiver).await else {
            panic!("a peer error")
        };
        assert_eq!(
            (
                error.request_id.as_str(),
                error.connection_generation.as_str()
            ),
            ("o1", "coord-generation")
        );
        assert_eq!(
            (error.worker_epoch.as_str(), error.reason.as_str()),
            (EPOCH, reason)
        );
        assert!(link.replies.is_empty());
    }
}

#[tokio::test]
async fn an_answer_for_a_superseded_coordinator_connection_is_dropped() {
    let direct = FakeDirect::new(Offer::Answer);
    let (dispatcher, mut receiver) = dispatcher(&direct);
    let mut link = FakeLink::default();
    receiver.advance();
    dispatcher.dispatch(offer_frame("before"), Instant::now(), &mut link);
    receiver.advance();
    settle_tasks().await;
    assert!(
        receiver.try_recv().is_none(),
        "the answer was fenced to the connection that asked"
    );
}
