//! The smoke offer faults at their real owner boundaries: each armed fault
//! is consumed by exactly the next offer and leaves the one after untouched.
//! Ports the offer injection sites of v2
//! `apps/worker/src/terminal/peer/terminal-peer-test-faults.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "peer_support/fake_native.rs"]
mod fake_native;
#[path = "peer_support/owner_fixture.rs"]
mod owner_fixture;

use std::sync::Arc;

use fake_native::FakeNative;
use owner_fixture::{Seen, WORKER_EPOCH, lock, offer, owner_with, peer_offer};
use roost_worker::peer::{OfferFault, OfferFaultSlot, TerminalPeerOfferFailure};

/// Each armed fault is consumed by exactly the next offer, at its real owner
/// boundary, and leaves the offer after it untouched.
#[tokio::test]
async fn every_offer_fault_fires_once_at_its_owner_boundary() {
    let fake = FakeNative::new();
    let seen = Arc::new(Seen::default());
    let faults = Arc::new(OfferFaultSlot::default());
    let owner = owner_with(&fake, fake.loader(), true, Some(Arc::clone(&faults)), &seen);

    faults.arm(OfferFault::InvalidSdp);
    assert_eq!(
        offer(&owner, peer_offer(1)).await,
        Err(TerminalPeerOfferFailure::InvalidOffer)
    );
    assert!(
        offer(&owner, peer_offer(2)).await.is_ok(),
        "one-shot: the next offer is untouched"
    );

    faults.arm(OfferFault::MissingGrant);
    assert_eq!(
        offer(&owner, peer_offer(3)).await,
        Err(TerminalPeerOfferFailure::GrantUnavailable)
    );

    faults.arm(OfferFault::ExpiredGrant);
    assert_eq!(
        offer(&owner, peer_offer(4)).await,
        Err(TerminalPeerOfferFailure::Expired)
    );
    assert_eq!(*lock(&seen.expired), vec!["grant-4".to_owned()]);

    faults.arm(OfferFault::IdentityMismatch);
    offer(&owner, peer_offer(5)).await.unwrap();
    let tuple = lock(&seen.tuples).last().cloned().unwrap();
    assert_eq!(tuple.worker_epoch, format!("{WORKER_EPOCH}-smoke-mismatch"));
    assert_eq!(faults.consume(), None);
    owner.dispose();
}
