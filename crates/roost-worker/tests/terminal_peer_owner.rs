//! Terminal peer owner: offer fencing, bounded negotiations, the expected
//! tuple bound before native SDP, and single-peer retirement — over the
//! deterministic native fake. Ports v2
//! `apps/worker/tests/terminal/peer/terminal-peer-owner.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;
#[path = "peer_support/owner_fixture.rs"]
mod owner_fixture;

use std::sync::{Arc, Mutex};

use fake_native::FakeNative;
use owner_fixture::{
    DEVICE, NoIngress, Seen, WORKER_EPOCH, budget, lock, offer, owner_with, peer_offer, settle,
    uuid,
};
use roost_proto::{DLocalTerminalPeerCancel, DLocalTerminalPeerOffer};
use roost_worker::local_terminal::{ExpectedPeer, PeerGrantAuthorization};
use roost_worker::peer::native::NativePeerEvent;
use roost_worker::peer::{
    PeerBootstrapState, PeerTransportConfig, TerminalPeerOfferFailure, TerminalPeerOwner,
    TerminalPeerOwnerDeps, TerminalPeerPacketBudget, TerminalPeerPacketIngress,
    TerminalPeerPacketPort,
};
use tokio::sync::Semaphore;

#[tokio::test]
async fn binds_the_expected_tuple_before_native_sdp_creates_fixed_channels_and_checks_dtls() {
    let fake = FakeNative::new();
    let seen = Arc::new(Seen::default());
    let owner = owner_with(&fake, fake.loader(), true, None, &seen);
    let request = peer_offer(1);
    let answer = offer(&owner, request.clone()).await.unwrap();
    assert_eq!(
        (answer.peer_id.as_str(), answer.worker_epoch.as_str()),
        (request.peer_id.as_str(), WORKER_EPOCH)
    );
    assert_eq!(answer.answer_sdp, fake_native::answer_sdp());
    let events = fake.events();
    let port_at = events
        .iter()
        .position(|event| *event == format!("port:{}", request.peer_id))
        .unwrap();
    let sdp_at = events
        .iter()
        .position(|event| event == "remote-description")
        .unwrap();
    assert!(
        port_at < sdp_at,
        "the port is bound before remote SDP: {events:?}"
    );
    let peer = &fake.peers()[0];
    let config = peer.config();
    assert_eq!(
        (
            config.bind_address,
            config.port_range,
            config.max_message_size
        ),
        (
            Some("127.0.0.1".parse().unwrap()),
            Some((41000, 41001)),
            16_384
        )
    );
    let channels: Vec<(u16, &str, bool, &str)> = config
        .channels
        .iter()
        .map(|spec| {
            (
                spec.id,
                spec.label.as_str(),
                spec.ordered,
                spec.protocol.as_str(),
            )
        })
        .collect();
    assert_eq!(
        channels,
        vec![
            (
                0,
                "roost-terminal-control-v1",
                true,
                "roost.local-terminal.v1"
            ),
            (1, "roost-terminal-data-v1", true, "roost.local-terminal.v1"),
            (
                2,
                "roost-terminal-history-v1",
                true,
                "roost.local-terminal.v1"
            ),
        ]
    );
    peer.emit(NativePeerEvent::Connected);
    for channel in 0..3 {
        peer.open_channel(channel);
    }
    settle().await;
    assert!(peer.remote_fingerprint_calls() > 0);
    assert!(!peer.is_closed(), "the offer's fingerprint matched");
    assert_eq!(
        *lock(&seen.tuples),
        vec![ExpectedPeer {
            peer_id: request.peer_id.clone(),
            grant_id: request.grant_id.clone(),
            device_fingerprint: request.device_fingerprint.clone(),
            tab_id: request.tab_id.clone(),
            worker_epoch: request.worker_epoch.clone(),
        }]
    );
    let same_actor = DLocalTerminalPeerOffer {
        tab_id: request.tab_id.clone(),
        ..peer_offer(2)
    };
    assert_eq!(
        offer(&owner, same_actor).await,
        Err(TerminalPeerOfferFailure::Capacity)
    );

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
    let disabled = owner_with(
        &fake,
        fake.gated_loader(Arc::clone(&loads)),
        false,
        None,
        &seen,
    );
    assert_eq!(disabled.bootstrap().await, PeerBootstrapState::Disabled);
    let unavailable = owner_with(&fake, FakeNative::failing_loader(), true, None, &seen);
    assert_eq!(
        unavailable.bootstrap().await,
        PeerBootstrapState::NativeUnavailable
    );
    let gate = Arc::new(Semaphore::new(0));
    let owner = owner_with(
        &fake,
        fake.gated_loader(Arc::clone(&gate)),
        true,
        None,
        &seen,
    );
    let stale = DLocalTerminalPeerOffer {
        worker_epoch: uuid(99),
        ..peer_offer(9)
    };
    assert_eq!(
        offer(&owner, stale).await,
        Err(TerminalPeerOfferFailure::ConnectionSuperseded)
    );

    let pending: Vec<DLocalTerminalPeerOffer> = (1..=4).map(peer_offer).collect();
    let futures: Vec<_> = pending
        .iter()
        .map(|request| {
            let (budget, fence) = budget();
            owner.offer(request.clone(), budget, fence)
        })
        .collect();
    assert_eq!(owner.negotiation_count(), 4);
    assert_eq!(
        offer(&owner, peer_offer(5)).await,
        Err(TerminalPeerOfferFailure::Capacity)
    );
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
        assert_eq!(
            future.await,
            Err(TerminalPeerOfferFailure::ConnectionSuperseded)
        );
    }
    owner.dispose();
    assert_eq!(fake.cleanup_calls(), 1);
    assert_eq!(
        loads.available_permits(),
        0,
        "a disabled owner never loads the transport"
    );
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
    assert_eq!(
        offer(&owner, peer_offer(33)).await,
        Err(TerminalPeerOfferFailure::Capacity)
    );
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
    assert_eq!(
        offer(&owner, peer_offer(1)).await,
        Err(TerminalPeerOfferFailure::IceFailed)
    );
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
            transport: PeerTransportConfig {
                enabled: true,
                bind_address: None,
                port_range: None,
            },
            is_current_coordinator: Arc::new(|_: &str| true),
            authorize_grant: Arc::new(|_: &DLocalTerminalPeerOffer| {
                PeerGrantAuthorization::Authorized
            }),
            open_peer_port: Arc::new(
                move |port: Arc<TerminalPeerPacketPort>, expected: ExpectedPeer| {
                    events.push_event(format!("port:{}", expected.peer_id));
                    lock(&ports).push(port);
                    Some(Arc::new(NoIngress) as Arc<dyn TerminalPeerPacketIngress>)
                },
            ),
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
