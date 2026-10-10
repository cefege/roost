//! The sticky "wants to pair" card: one per pending request however it
//! arrives, raised only when the request is new, carrying the Review button
//! that opens the approvals, and taken away when the request leaves the set.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::SyncDomain;
use roost_client_core::store::toasts::{ToastIntent, dismiss_toast, pair_request_toast_id};
use roost_proto::__buffa::oneof::pair_request_delta_proto::Kind as PairKind;
use roost_proto::PairRequestsSnapshot;

use sync_decode_support::pair::{completed, pair_arm, pending, toast_messages};
use sync_decode_support::{application, deliver, ready_core};

const CARD: &str = "Chrome — macOS wants to pair with Roost";

#[test]
fn a_new_pair_request_raises_one_sticky_review_card() {
    let (mut core, generation) = ready_core();
    for seq in [1, 2] {
        let arm = pair_arm(PairKind::Pending(Box::new(pending("a"))));
        deliver(
            &mut core,
            generation,
            &application(SyncDomain::Pair, seq, arm),
        );
    }
    assert_eq!(toast_messages(&core), [CARD]);
    let id = pair_request_toast_id("a");
    let card = core
        .store()
        .toasts
        .toasts()
        .find(|toast| toast.id == id)
        .expect("the card");
    assert_eq!(
        card.action.as_ref().map(|action| &action.intent),
        Some(&ToastIntent::OpenPairApprovals)
    );
    assert_eq!(card.expires_at_ms, None, "the card stays until handled");

    dismiss_toast(core.store_mut(), &id);
    let arm = pair_arm(PairKind::Pending(Box::new(pending("a"))));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 3, arm),
    );
    assert!(
        toast_messages(&core).is_empty(),
        "a request already held does not bring a dismissed card back"
    );
}

#[test]
fn a_removed_or_completed_request_takes_its_card_away() {
    let (mut core, generation) = ready_core();
    let arm = pair_arm(PairKind::Pending(Box::new(pending("a"))));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 1, arm),
    );
    let removed = pair_arm(PairKind::RemovedId("a".to_owned()));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 2, removed),
    );
    assert!(
        !toast_messages(&core)
            .iter()
            .any(|message| message.contains("wants to pair")),
        "a removed request keeps no card"
    );

    let arm = pair_arm(PairKind::Pending(Box::new(pending("b"))));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 3, arm),
    );
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 4, completed("b")),
    );
    assert_eq!(
        toast_messages(&core),
        ["New browser paired: Chrome on macOS · Berlin"]
    );
}

#[test]
fn a_pair_snapshot_raises_new_cards_and_drops_vanished_ones() {
    let (mut core, generation) = ready_core();
    let arm = pair_arm(PairKind::Pending(Box::new(pending("a"))));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 1, arm),
    );
    let snapshot = pair_arm(PairKind::Snapshot(Box::new(PairRequestsSnapshot {
        pending: vec![pending("b")],
        ..PairRequestsSnapshot::default()
    })));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Pair, 2, snapshot),
    );
    assert_eq!(toast_messages(&core), [CARD]);
    assert!(
        core.store()
            .toasts
            .toasts()
            .all(|toast| toast.id == pair_request_toast_id("b")),
        "only b's card remains"
    );
}
