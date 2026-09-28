//! Terminal peer owner: offer fencing, bounded negotiations, the expected
//! tuple bound before native SDP, and single-peer retirement — over the
//! deterministic native fake. Ports v2
//! `apps/worker/tests/terminal/peer/terminal-peer-owner.test.ts`; the offer
//! fault arms are v2 `terminal-peer-test-faults.ts`'s injection sites.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use fake_native::{FakeNative, offer_sdp};
use roost_proto::{DLocalTerminalPeerCancel, DLocalTerminalPeerOffer};
use roost_worker::local_terminal::{ExpectedPeer, PeerGrantAuthorization};
use roost_worker::peer::native::{NativeLoader, NativePeerEvent};
use roost_worker::peer::{
    OfferFault, OfferFaultSlot, PeerBootstrapState, PeerTransportConfig, TerminalPeerOfferFailure,
    TerminalPeerOwner, TerminalPeerOwnerDeps, TerminalPeerPacketBudget, TerminalPeerPacketIngress,
    TerminalPeerPacketPort,
};
use roost_worker::uplink::{RequestBudget, Uplink};
use tokio::sync::Semaphore;

const WORKER_EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const DEVICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

struct NoIngress;

impl TerminalPeerPacketIngress for NoIngress {
    fn on_message(&self, _: &[u8]) {}
    fn on_close(&self) {}
}

fn uuid(tail: u32) -> String {
    format!("00000000-0000-4000-8000-{tail:012x}")
}

fn peer_offer(serial: u32) -> DLocalTerminalPeerOffer {
    DLocalTerminalPeerOffer {
        request_id: format!("request-{serial}"),
        connection_generation: format!("generation-{serial}"),
        worker_epoch: WORKER_EPOCH.into(),
        grant_id: format!("grant-{serial}"),
        peer_id: uuid(serial),
        device_fingerprint: DEVICE.into(),
        tab_id: format!("tab-{serial}"),
        offer_sdp: offer_sdp(),
        budget_ms: 8_000,
        ..Default::default()
    }
}

/// The records an owner's injected dependencies keep.
#[derive(Default)]
struct Seen {
    tuples: Mutex<Vec<ExpectedPeer>>,
    expired: Mutex<Vec<String>>,
}

fn owner_with(fake: &Arc<FakeNative>, loader: NativeLoader, enabled: bool, faults: Option<Arc<OfferFaultSlot>>, seen: &Arc<Seen>) -> Arc<TerminalPeerOwner> {
    let (tuples, expired, grants) = (Arc::clone(seen), Arc::clone(seen), Arc::clone(seen));
    let events = Arc::clone(fake);
    TerminalPeerOwner::new(TerminalPeerOwnerDeps {
        process_epoch: WORKER_EPOCH.into(),
        transport: PeerTransportConfig { enabled, bind_address: Some("127.0.0.1".parse().unwrap()), port_range: Some((41000, 41001)) },
        is_current_coordinator: Arc::new(|_: &str| true),
        // v2 `grants.authorizePeer`: an unknown id is unavailable, a removed-as-expired one expired.
        authorize_grant: Arc::new(move |request: &DLocalTerminalPeerOffer| {
            if lock(&grants.expired).contains(&request.grant_id) {
                PeerGrantAuthorization::Expired
            } else if request.grant_id.starts_with("grant-") {
                PeerGrantAuthorization::Authorized
            } else {
                PeerGrantAuthorization::GrantUnavailable
            }
        }),
        open_peer_port: Arc::new(move |_port: Arc<TerminalPeerPacketPort>, expected: ExpectedPeer| {
            events.push_event(format!("port:{}", expected.peer_id));
            lock(&tuples.tuples).push(expected);
            Some(Arc::new(NoIngress) as Arc<dyn TerminalPeerPacketIngress>)
        }),
        native_loader: loader,
        packet_budget: TerminalPeerPacketBudget::new(),
        offer_faults: faults,
        expire_grant: Arc::new(move |grant_id: &str| lock(&expired.expired).push(grant_id.to_owned())),
        runtime: tokio::runtime::Handle::current(),
    })
}

fn budget() -> (RequestBudget, roost_worker::uplink::LinkFence) {
    (RequestBudget::from_budget_ms(8_000, Instant::now()), Uplink::detached().fence())
}

async fn offer(owner: &Arc<TerminalPeerOwner>, request: DLocalTerminalPeerOffer) -> Result<roost_proto::WLocalTerminalPeerAnswer, TerminalPeerOfferFailure> {
    let (budget, fence) = budget();
    owner.offer(request, budget, fence).await
}

async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn binds_the_expected_tuple_before_native_sdp_creates_fixed_channels_and_checks_dtls() {
    let fake = FakeNative::new();
    let seen = Arc::new(Seen::default());
    let owner = owner_with(&fake, fake.loader(), true, None, &seen);
    let request = peer_offer(1);
    let answer = offer(&owner, request.clone()).await.unwrap();
    assert_eq!((answer.peer_id.as_str(), answer.worker_epoch.as_str()), (request.peer_id.as_str(), WORKER_EPOCH));
    assert_eq!(answer.answer_sdp, fake_native::answer_sdp());
    let events = fake.events();
    let port_at = events.iter().position(|event| *event == format!("port:{}", request.peer_id)).unwrap();
    let sdp_at = events.iter().position(|event| event == "remote-description").unwrap();
    assert!(port_at < sdp_at, "the port is bound before remote SDP: {events:?}");
    let peer = &fake.peers()[0];
    let config = peer.config();
    assert_eq!((config.bind_address, config.port_range, config.max_message_size), (Some("127.0.0.1".parse().unwrap()), Some((41000, 41001)), 16_384));
    let channels: Vec<(u16, &str, bool, &str)> = config.channels.iter().map(|spec| (spec.id, spec.label.as_str(), spec.ordered, spec.protocol.as_str())).collect();
    assert_eq!(channels, vec![
        (0, "roost-terminal-control-v1", true, "roost.local-terminal.v1"),
        (1, "roost-terminal-data-v1", true, "roost.local-terminal.v1"),
        (2, "roost-terminal-history-v1", true, "roost.local-terminal.v1"),
    ]);
    peer.emit(NativePeerEvent::Connected);
    for channel in 0..3 {
        peer.open_channel(channel);
    }
    settle().await;
    assert!(peer.remote_fingerprint_calls() > 0);
    assert!(!peer.is_closed(), "the offer's fingerprint matched");
    assert_eq!(*lock(&seen.tuples), vec![ExpectedPeer {
        peer_id: request.peer_id.clone(),
        grant_id: request.grant_id.clone(),
        device_fingerprint: request.device_fingerprint.clone(),
        tab_id: request.tab_id.clone(),
        worker_epoch: request.worker_epoch.clone(),
    }]);
    let same_actor = DLocalTerminalPeerOffer { tab_id: request.tab_id.clone(), ..peer_offer(2) };
    assert_eq!(offer(&owner, same_actor).await, Err(TerminalPeerOfferFailure::Capacity));

    owner.revoke_device(DEVICE);
    assert!(peer.is_closed());
    assert_eq!(owner.established_count(), 0);
    owner.dispose();
    owner.dispose();
    assert_eq!(fake.cleanup_calls(), 1);
}

#[tokio::test]
async fn reports_bootstrap_states_and_bounds_pending_offers() {
    let fake = FakeNative::new();
    let seen = Arc::new(Seen::default());
    let loads = Arc::new(Semaphore::new(0));
    let disabled = owner_with(&fake, fake.gated_loader(Arc::clone(&loads)), false, None, &seen);
    assert_eq!(disabled.bootstrap().await, PeerBootstrapState::Disabled);
    let unavailable = owner_with(&fake, FakeNative::failing_loader(), true, None, &seen);
    assert_eq!(unavailable.bootstrap().await, PeerBootstrapState::NativeUnavailable);
    let gate = Arc::new(Semaphore::new(0));
    let owner = owner_with(&fake, fake.gated_loader(Arc::clone(&gate)), true, None, &seen);
    let stale = DLocalTerminalPeerOffer { worker_epoch: uuid(99), ..peer_offer(9) };
    assert_eq!(offer(&owner, stale).await, Err(TerminalPeerOfferFailure::ConnectionSuperseded));

    let pending: Vec<DLocalTerminalPeerOffer> = (1..=4).map(peer_offer).collect();
    let futures: Vec<_> = pending.iter().map(|request| {
        let (budget, fence) = budget();
        owner.offer(request.clone(), budget, fence)
    }).collect();
    assert_eq!(owner.negotiation_count(), 4);
    assert_eq!(offer(&owner, peer_offer(5)).await, Err(TerminalPeerOfferFailure::Capacity));
    for request in &pending {
        owner.cancel(&DLocalTerminalPeerCancel {
            request_id: request.request_id.clone(),
            connection_generation: request.connection_generation.clone(),
            worker_epoch: request.worker_epoch.clone(),
            peer_id: request.peer_id.clone(),
            ..Default::default()
        });
    }
    gate.add_permits(1);
    for future in futures {
        assert_eq!(future.await, Err(TerminalPeerOfferFailure::ConnectionSuperseded));
    }
    owner.dispose();
    assert_eq!(fake.cleanup_calls(), 1);
    assert_eq!(loads.available_permits(), 0, "a disabled owner never loads the transport");
}

#[tokio::test]
async fn reserves_the_final_established_slot_while_an_answer_is_still_pending() {
    let fake = FakeNative::new();
    let owner = owner_with(&fake, fake.loader(), true, None, &Arc::default());
    for serial in 1..=31 {
        offer(&owner, peer_offer(serial)).await.unwrap();
    }
    fake.set_defer_new_peers(true);
    let (budget, fence) = budget();
    let held = tokio::spawn(owner.offer(peer_offer(32), budget, fence));
    settle().await;
    assert_eq!(owner.negotiation_count(), 1);
    assert_eq!(offer(&owner, peer_offer(33)).await, Err(TerminalPeerOfferFailure::Capacity));
    fake.peers().last().unwrap().complete_gathering();
    held.await.unwrap().unwrap();
    assert_eq!(owner.established_count(), 32);
    owner.dispose();
}

#[tokio::test]
async fn does_not_promote_an_answer_whose_peer_closes_before_the_offer_continuation() {
    let fake = FakeNative::new();
    fake.set_fail_after_answer(true);
    let owner = owner_with(&fake, fake.loader(), true, None, &Arc::default());
    assert_eq!(offer(&owner, peer_offer(1)).await, Err(TerminalPeerOfferFailure::IceFailed));
    assert_eq!(owner.established_count(), 0);
    owner.dispose();
}

#[tokio::test]
async fn retires_one_peer_for_an_injected_port_close_or_native_callback() {
    let fake = FakeNative::new();
    let ports = Arc::new(Mutex::new(Vec::new()));
    let owner = {
        let ports = Arc::clone(&ports);
        let events = Arc::clone(&fake);
        TerminalPeerOwner::new(TerminalPeerOwnerDeps {
            process_epoch: WORKER_EPOCH.into(),
            transport: PeerTransportConfig { enabled: true, bind_address: None, port_range: None },
            is_current_coordinator: Arc::new(|_: &str| true),
            authorize_grant: Arc::new(|_: &DLocalTerminalPeerOffer| PeerGrantAuthorization::Authorized),
            open_peer_port: Arc::new(move |port: Arc<TerminalPeerPacketPort>, expected: ExpectedPeer| {
                events.push_event(format!("port:{}", expected.peer_id));
                lock(&ports).push(port);
                Some(Arc::new(NoIngress) as Arc<dyn TerminalPeerPacketIngress>)
            }),
            native_loader: fake.loader(),
            packet_budget: TerminalPeerPacketBudget::new(),
            offer_faults: None,
            expire_grant: Arc::new(|_: &str| {}),
            runtime: tokio::runtime::Handle::current(),
        })
    };
    offer(&owner, peer_offer(1)).await.unwrap();
    offer(&owner, peer_offer(2)).await.unwrap();
    assert_eq!(owner.established_count(), 2);
    let first_port = Arc::clone(&lock(&ports)[0]);
    roost_worker::local_terminal::TerminalPacketPort::close(first_port.as_ref(), 1000, "test");
    let peers = fake.peers();
    assert!(peers[0].is_closed());
    assert!(!peers[1].is_closed());
    assert_eq!(owner.established_count(), 1);
    peers[1].emit(NativePeerEvent::UnsolicitedChannel);
    peers[1].emit(NativePeerEvent::UnsolicitedChannel);
    settle().await;
    assert!(peers[1].is_closed());
    assert_eq!(owner.established_count(), 0);
    owner.dispose();
}

/// Each armed fault is consumed by exactly the next offer, at its real owner
/// boundary, and leaves the offer after it untouched.
#[tokio::test]
async fn every_offer_fault_fires_once_at_its_owner_boundary() {
    let fake = FakeNative::new();
    let seen = Arc::new(Seen::default());
    let faults = Arc::new(OfferFaultSlot::default());
    let owner = owner_with(&fake, fake.loader(), true, Some(Arc::clone(&faults)), &seen);

    faults.arm(OfferFault::InvalidSdp);
    assert_eq!(offer(&owner, peer_offer(1)).await, Err(TerminalPeerOfferFailure::InvalidOffer));
    assert!(offer(&owner, peer_offer(2)).await.is_ok(), "one-shot: the next offer is untouched");

    faults.arm(OfferFault::MissingGrant);
    assert_eq!(offer(&owner, peer_offer(3)).await, Err(TerminalPeerOfferFailure::GrantUnavailable));

    faults.arm(OfferFault::ExpiredGrant);
    assert_eq!(offer(&owner, peer_offer(4)).await, Err(TerminalPeerOfferFailure::Expired));
    assert_eq!(*lock(&seen.expired), vec!["grant-4".to_owned()]);

    faults.arm(OfferFault::IdentityMismatch);
    offer(&owner, peer_offer(5)).await.unwrap();
    let tuple = lock(&seen.tuples).last().cloned().unwrap();
    assert_eq!(tuple.worker_epoch, format!("{WORKER_EPOCH}-smoke-mismatch"));
    assert_eq!(faults.consume(), None);
    owner.dispose();
}
