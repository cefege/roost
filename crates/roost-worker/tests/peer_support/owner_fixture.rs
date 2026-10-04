//! The terminal peer owner over the deterministic native fake, and the
//! records its injected grant, expiry and port dependencies keep. Included by
//! `terminal_peer_owner.rs` and `terminal_peer_offer_faults.rs`. Ports v2
//! `apps/worker/tests/terminal/peer/terminal-peer-owner-fixture.ts` (owner half).
#![allow(dead_code)]

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use roost_proto::DLocalTerminalPeerOffer;
use roost_worker::local_terminal::{ExpectedPeer, PeerGrantAuthorization};
use roost_worker::peer::native::NativeLoader;
use roost_worker::peer::{
    PeerTestFaults, PeerTransportConfig, TerminalPeerOfferFailure, TerminalPeerOwner,
    TerminalPeerOwnerDeps, TerminalPeerPacketBudget, TerminalPeerPacketIngress,
    TerminalPeerPacketPort,
};
use roost_worker::uplink::{RequestBudget, Uplink};

use super::fake_native::{FakeNative, offer_sdp};

pub const WORKER_EPOCH: &str = "11111111-1111-4111-8111-111111111111";
pub const DEVICE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

pub struct NoIngress;

impl TerminalPeerPacketIngress for NoIngress {
    fn on_message(&self, _: &[u8]) {}
    fn on_close(&self) {}
}

pub fn uuid(tail: u32) -> String {
    format!("00000000-0000-4000-8000-{tail:012x}")
}

pub fn peer_offer(serial: u32) -> DLocalTerminalPeerOffer {
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
pub struct Seen {
    pub tuples: Mutex<Vec<ExpectedPeer>>,
    pub expired: Mutex<Vec<String>>,
}

pub fn owner_with(
    fake: &Arc<FakeNative>,
    loader: NativeLoader,
    enabled: bool,
    faults: Option<Arc<PeerTestFaults>>,
    seen: &Arc<Seen>,
) -> Arc<TerminalPeerOwner> {
    let (tuples, expired, grants) = (Arc::clone(seen), Arc::clone(seen), Arc::clone(seen));
    let events = Arc::clone(fake);
    TerminalPeerOwner::new(TerminalPeerOwnerDeps {
        process_epoch: WORKER_EPOCH.into(),
        transport: PeerTransportConfig {
            enabled,
            bind_address: Some("127.0.0.1".parse().unwrap()),
            port_range: Some((41000, 41001)),
        },
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
        open_peer_port: Arc::new(
            move |_port: Arc<TerminalPeerPacketPort>, expected: ExpectedPeer| {
                events.push_event(format!("port:{}", expected.peer_id));
                lock(&tuples.tuples).push(expected);
                Some(Arc::new(NoIngress) as Arc<dyn TerminalPeerPacketIngress>)
            },
        ),
        native_loader: loader,
        packet_budget: TerminalPeerPacketBudget::new(),
        test_faults: faults,
        expire_grant: Arc::new(move |grant_id: &str| {
            lock(&expired.expired).push(grant_id.to_owned())
        }),
        runtime: tokio::runtime::Handle::current(),
    })
}

pub fn budget() -> (RequestBudget, roost_worker::uplink::LinkFence) {
    (
        RequestBudget::from_budget_ms(8_000, Instant::now()),
        Uplink::detached().fence(),
    )
}

pub async fn offer(
    owner: &Arc<TerminalPeerOwner>,
    request: DLocalTerminalPeerOffer,
) -> Result<roost_proto::WLocalTerminalPeerAnswer, TerminalPeerOfferFailure> {
    let (budget, fence) = budget();
    owner.offer(request, budget, fence).await
}

pub async fn settle() {
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
}
