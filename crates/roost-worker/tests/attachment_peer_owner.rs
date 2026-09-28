//! Attachment peer ownership stays isolated from terminal peer ownership even
//! when both borrow one native runtime: a failed attachment connection retires
//! only attachment transport state, never a terminal connection. Ported from
//! `apps/worker/tests/attachments/attachment-peer-owner.test.ts` over the shared
//! fake native (`peer_support/fake_native.rs`, v2 `createFakeNativeFixture`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;

use std::sync::Arc;
use std::time::{Duration, Instant};

use fake_native::{FakeNative, offer_sdp};
use roost_proto::{DLocalAttachmentPeerOffer, DLocalTerminalPeerOffer};
use roost_protocol::attachment_transfer::PeerChannelLane;
use roost_worker::attachments::grants::PeerGrantAuthorization;
use roost_worker::attachments::peer_budget::AttachmentPeerPacketBudget;
use roost_worker::attachments::peer_owner::{AttachmentPeerOwner, AttachmentPeerOwnerDeps};
use roost_worker::attachments::peer_packet_port::{
    AttachmentPeerIngress, AttachmentPeerPacketPort,
};
use roost_worker::attachments::transfer_admission::AttachmentPeerExpectedTuple;
use roost_worker::local_terminal::ExpectedPeer;
use roost_worker::peer::native::NativePeerEvent;
use roost_worker::peer::{
    PeerTransportConfig, TerminalPeerOwner, TerminalPeerOwnerDeps, TerminalPeerPacketIngress,
    TerminalPeerPacketPort,
};
use roost_worker::session::ids::mint_uuid;
use roost_worker::uplink::{RequestBudget, Uplink};

const WORKER_EPOCH: &str = "11111111-1111-4111-8111-111111111111";
const DEVICE: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

#[derive(Debug)]
struct IgnoredAttachmentFrames;

impl AttachmentPeerIngress for IgnoredAttachmentFrames {
    fn on_message(&self, _lane: PeerChannelLane, _bytes: Vec<u8>) {}
    fn on_close(&self) {}
}

#[derive(Debug)]
struct IgnoredTerminalFrames;

impl TerminalPeerPacketIngress for IgnoredTerminalFrames {
    fn on_message(&self, _bytes: &[u8]) {}
    fn on_close(&self) {}
}

fn live_budget() -> RequestBudget {
    RequestBudget::from_budget_ms(8_000, Instant::now())
}

fn enabled() -> PeerTransportConfig {
    PeerTransportConfig {
        enabled: true,
        bind_address: None,
        port_range: None,
    }
}

#[tokio::test]
async fn attachment_peer_failure_leaves_the_terminal_peer_established() {
    let fake = FakeNative::new();
    let terminal_owner = TerminalPeerOwner::new(TerminalPeerOwnerDeps {
        process_epoch: WORKER_EPOCH.to_owned(),
        transport: enabled(),
        is_current_coordinator: Arc::new(|_: &str| true),
        authorize_grant: Arc::new(|_: &DLocalTerminalPeerOffer| {
            roost_worker::local_terminal::PeerGrantAuthorization::Authorized
        }),
        open_peer_port: Arc::new(|_: Arc<TerminalPeerPacketPort>, _: ExpectedPeer| {
            Some(Arc::new(IgnoredTerminalFrames) as Arc<dyn TerminalPeerPacketIngress>)
        }),
        native_loader: fake.loader(),
        packet_budget: Default::default(),
        offer_faults: None,
        expire_grant: Arc::new(|_: &str| {}),
        runtime: tokio::runtime::Handle::current(),
    });
    let attachment_owner = AttachmentPeerOwner::new(AttachmentPeerOwnerDeps {
        process_epoch: WORKER_EPOCH.to_owned(),
        enabled: true,
        bind_address: None,
        port_range: None,
        is_current_coordinator: Arc::new(|_: &str| true),
        authorize_grant: Arc::new(|_: &DLocalAttachmentPeerOffer| {
            PeerGrantAuthorization::Authorized
        }),
        open_peer_port: Arc::new(
            |_: Arc<AttachmentPeerPacketPort>, _: AttachmentPeerExpectedTuple| {
                Some(Arc::new(IgnoredAttachmentFrames) as Arc<dyn AttachmentPeerIngress>)
            },
        ),
        native_loader: fake.loader(),
        packet_budget: AttachmentPeerPacketBudget::new(),
    });
    let terminal_offer = DLocalTerminalPeerOffer {
        request_id: mint_uuid().unwrap(),
        connection_generation: mint_uuid().unwrap(),
        worker_epoch: WORKER_EPOCH.to_owned(),
        grant_id: mint_uuid().unwrap(),
        peer_id: mint_uuid().unwrap(),
        device_fingerprint: DEVICE.to_owned(),
        tab_id: mint_uuid().unwrap(),
        offer_sdp: offer_sdp(),
        budget_ms: 8_000,
        ..Default::default()
    };
    let attachment_offer = DLocalAttachmentPeerOffer {
        request_id: mint_uuid().unwrap(),
        connection_generation: mint_uuid().unwrap(),
        worker_epoch: WORKER_EPOCH.to_owned(),
        grant_id: mint_uuid().unwrap(),
        peer_id: mint_uuid().unwrap(),
        device_fingerprint: DEVICE.to_owned(),
        tab_id: mint_uuid().unwrap(),
        offer_sdp: offer_sdp(),
        budget_ms: 8_000,
        ..Default::default()
    };
    let fence = Uplink::detached().fence();

    terminal_owner
        .offer(terminal_offer, live_budget(), fence.clone())
        .await
        .unwrap();
    attachment_owner
        .offer(attachment_offer, live_budget(), fence)
        .await
        .unwrap();
    assert_eq!(terminal_owner.established_count(), 1);
    assert_eq!(attachment_owner.established_count(), 1);

    fake.peers()[1].emit(NativePeerEvent::Failed);
    tokio::time::timeout(Duration::from_secs(5), async {
        while attachment_owner.established_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the failed attachment connection is retired");

    assert_eq!(terminal_owner.established_count(), 1);
    assert!(!fake.peers()[0].is_closed());
    attachment_owner.dispose();
    terminal_owner.dispose();
}
