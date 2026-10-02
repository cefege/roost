//! The direct terminal path as the coordinator link drives it, over the real
//! door and grant store: one coordinator generation is adopted per link and
//! released on detach, a transport probe answers only this process's epoch, a
//! direct retire honours only v2's two reasons for this epoch and disposes the
//! grants and the peer owner once, and a device revoke reaches the door. Ports
//! v2 `apps/worker/src/transport/coord-link-direct-deps.ts` (terminal half) and
//! `boot/boot-local-terminal.ts` (`useCoordinatorGeneration`, `disposeDirect`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;
mod local_terminal_support;
mod terminal_stream_support;

use std::sync::Arc;
use std::time::Instant;

use fake_native::FakeNative;
use local_terminal_support::{DEVICE, Fixture, GRANT_ID, TAB, WORKER_EPOCH};
use roost_proto::{
    DLocalTerminalPeerCancel, DLocalTerminalPeerOffer, DTerminalDirectRetire,
    DTerminalTransportProbe,
};
use roost_worker::link_ports::{DirectTerminalPort, LocalTerminalGrantPort};
use roost_worker::peer::{
    DirectTerminal, DirectTerminalDeps, PeerBootstrapState, PeerTransportConfig,
    TerminalPeerOfferFailure,
};
use roost_worker::uplink::{RequestBudget, Uplink};

fn direct(fixture: &Fixture, fake: &Arc<FakeNative>) -> Arc<DirectTerminal> {
    DirectTerminal::new(DirectTerminalDeps {
        door: Arc::clone(&fixture.door),
        process_epoch: WORKER_EPOCH.to_owned(),
        transport: PeerTransportConfig {
            enabled: true,
            bind_address: None,
            port_range: None,
        },
        native_loader: fake.loader(),
        test_faults: None,
        runtime: tokio::runtime::Handle::current(),
    })
}

/// An offer the door's grant authorizes and whose SDP the owner refuses: it
/// reaches the owner (`invalid_offer`) only past the generation gate.
fn offer(generation: &str) -> DLocalTerminalPeerOffer {
    DLocalTerminalPeerOffer {
        request_id: format!("offer-{generation}"),
        connection_generation: generation.to_owned(),
        worker_epoch: WORKER_EPOCH.to_owned(),
        grant_id: GRANT_ID.to_owned(),
        peer_id: "00000000-0000-4000-8000-000000000001".to_owned(),
        device_fingerprint: DEVICE.to_owned(),
        tab_id: TAB.to_owned(),
        offer_sdp: "not an sdp offer".to_owned(),
        budget_ms: 5_000,
        ..Default::default()
    }
}

async fn offer_outcome(direct: &DirectTerminal, generation: &str) -> TerminalPeerOfferFailure {
    let budget = RequestBudget::from_budget_ms(5_000, Instant::now());
    direct
        .peer_offer(offer(generation), budget, Uplink::detached().fence())
        .await
        .expect_err("a malformed offer is refused")
}

#[tokio::test]
async fn one_coordinator_generation_is_adopted_per_link_and_released_on_detach() {
    let fixture = Fixture::new();
    let direct = direct(&fixture, &FakeNative::new());
    // v2: a cancel adopts the generation exactly as an offer does.
    direct.peer_cancel(&DLocalTerminalPeerCancel {
        request_id: "cancel-a".to_owned(),
        connection_generation: "generation-a".to_owned(),
        ..Default::default()
    });
    assert_eq!(
        offer_outcome(&direct, "generation-b").await,
        TerminalPeerOfferFailure::ConnectionSuperseded
    );
    assert_eq!(
        offer_outcome(&direct, "").await,
        TerminalPeerOfferFailure::ConnectionSuperseded
    );
    assert_eq!(
        offer_outcome(&direct, "generation-a").await,
        TerminalPeerOfferFailure::InvalidOffer
    );

    direct.coordinator_detached();
    assert_eq!(
        offer_outcome(&direct, "generation-b").await,
        TerminalPeerOfferFailure::InvalidOffer
    );
    assert_eq!(
        offer_outcome(&direct, "generation-a").await,
        TerminalPeerOfferFailure::ConnectionSuperseded
    );
}

#[tokio::test]
async fn a_transport_probe_is_answered_only_for_this_process_epoch() {
    let fixture = Fixture::new();
    let direct = direct(&fixture, &FakeNative::new());
    let probe = |worker_epoch: &str| DTerminalTransportProbe {
        request_id: "probe-1".to_owned(),
        worker_epoch: worker_epoch.to_owned(),
        ..Default::default()
    };
    let answered = direct
        .transport_probe(&probe(WORKER_EPOCH))
        .expect("this epoch is answered");
    assert_eq!(
        (answered.request_id.as_str(), answered.worker_epoch.as_str()),
        ("probe-1", WORKER_EPOCH)
    );
    assert!(
        direct
            .transport_probe(&probe("22222222-2222-4222-8222-222222222222"))
            .is_none()
    );
}

#[tokio::test]
async fn a_direct_retire_disposes_only_for_this_epoch_and_v2s_two_reasons() {
    let fixture = Fixture::new();
    let fake = FakeNative::new();
    let direct = direct(&fixture, &fake);
    assert_eq!(
        direct.peer_owner().bootstrap().await,
        PeerBootstrapState::Ready
    );
    let retire = |worker_epoch: &str, reason: &str| DTerminalDirectRetire {
        worker_epoch: worker_epoch.to_owned(),
        reason: reason.to_owned(),
        ..Default::default()
    };
    direct.direct_retire(&retire(
        "22222222-2222-4222-8222-222222222222",
        "worker_deleted",
    ));
    direct.direct_retire(&retire(WORKER_EPOCH, "coordinator_restart"));
    assert!(
        fixture.grants.current(GRANT_ID).is_some(),
        "a foreign or unknown retire keeps the grants"
    );
    assert_eq!(
        direct.peer_owner().bootstrap().await,
        PeerBootstrapState::Ready
    );

    direct.direct_retire(&retire(WORKER_EPOCH, "worker_revoked"));
    assert!(
        fixture.grants.current(GRANT_ID).is_none(),
        "the retire disposed the door's grants"
    );
    assert_eq!(
        direct.peer_owner().bootstrap().await,
        PeerBootstrapState::NativeUnavailable
    );
    direct.direct_retire(&retire(WORKER_EPOCH, "worker_deleted"));
    assert_eq!(
        fake.cleanup_calls(),
        1,
        "the native transport is released once"
    );
}

#[tokio::test]
async fn a_device_revoke_reaches_the_doors_grants() {
    let fixture = Fixture::new();
    let direct = direct(&fixture, &FakeNative::new());
    direct.revoke_device("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee");
    assert!(
        fixture.grants.current(GRANT_ID).is_some(),
        "another device's revoke keeps this grant"
    );
    direct.revoke_device(DEVICE);
    assert!(fixture.grants.current(GRANT_ID).is_none());
}
