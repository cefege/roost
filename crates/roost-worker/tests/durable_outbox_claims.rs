//! The durable outbox's CAPACITY CLAIMS, at the store level. A session claims
//! the room its `closed` event will need BEFORE the PTY exists, so a spawn that
//! cannot record its end never opens one — which is only worth anything if the
//! claim outlives the process that took it, and is handed back once that process
//! is gone rather than held for ever.
//!
//! The two failure modes are opposites, and the persistence is what makes them
//! trade places: a claim held only in a heap is silently gone after a reboot,
//! while a claim held in a FILE by a process that died is capacity nobody hands
//! back. So the claim is stamped, and only the ones past the lease are swept.
//!
//! The rows' own survival is `durable_outbox.rs`, and the volatile producers'
//! fold is `durable_outbox_coalescing.rs`; all three were one suite until it
//! outgrew one file.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "outbox_support/mod.rs"]
mod support;

use roost_worker::event_store::DurableEventKind;

use support::{SESSION, Scratch, journal_in, opened};

/// THE POINT OF PERSISTING IT. A session claims the room its `closed` event will
/// need BEFORE the PTY exists, so a spawn that cannot record its end never opens
/// one. A claim held only in a heap is silently gone after a reboot, and the
/// store then admits an `opened` it has no room to close.
#[tokio::test]
async fn a_claim_survives_a_close_and_reopen() {
    let scratch = Scratch::new("claim-survives");
    let first = {
        let journal = journal_in(&scratch).await;
        let claim = journal
            .reserve(DurableEventKind::Closed, 2 * 1024)
            .await
            .expect("a fresh outbox has room");
        (claim.id(), claim.kind())
    };
    let journal = journal_in(&scratch).await;
    assert_eq!(
        journal.claims().await.expect("read"),
        (1, 1),
        "the claim was gone, so a session that had taken room for its close no longer holds any"
    );
    // A second claim must not reuse the first one's id: a claim that came back
    // with an identity already in use is a claim the first session and the
    // second both own, which is the failure the token exists to prevent.
    let second = journal
        .reserve(DurableEventKind::Opened, 2 * 1024)
        .await
        .expect("room remains");
    assert_ne!(second.id(), first.0, "a claim id was handed out twice");
    assert_eq!(second.kind(), DurableEventKind::Opened);
    assert_eq!(first.1, DurableEventKind::Closed);
    let held = journal.hold(second).await.expect("held");
    journal.release(held).await.expect("released");
    assert_eq!(
        journal.claims().await.expect("read"),
        (1, 1),
        "the released claim is still there"
    );
}

/// THE LEASE. A claim from a process that died is capacity nobody hands back,
/// and in a FILE it would never be handed back at all — so the store reclaims
/// the ones past the lease, and only those.
#[tokio::test]
async fn a_claim_past_the_lease_is_reclaimed_and_one_inside_it_is_not() {
    let scratch = Scratch::new("claim-lease");
    let journal = journal_in(&scratch).await;
    let stale = journal
        .reserve(DurableEventKind::Closed, 2 * 1024)
        .await
        .expect("a fresh outbox has room");
    let fresh = journal
        .reserve(DurableEventKind::Opened, 2 * 1024)
        .await
        .expect("room remains");

    // The store's OWN clock, which is a hair after the two reserves and
    // therefore well inside the lease. Nothing goes: a sweep that took a live
    // spawn's claim would leave a session mid-spawn with no room for its close,
    // and no error anywhere to say so.
    assert_eq!(
        journal
            .reclaim_expired_claims(now_ms())
            .await
            .expect("swept"),
        0,
        "a live spawn's claim was reclaimed, so a session mid-spawn lost the room it had already \
         taken for its close"
    );
    assert_eq!(journal.claims().await.expect("read"), (2, 2));

    // A clock far enough ahead that both claims are unambiguously past it. Both
    // go, and the store is empty — which is the failure the lease exists to
    // prevent: capacity held for ever by processes that are gone.
    let reclaimed = journal
        .reclaim_expired_claims(i64::MAX / 2)
        .await
        .expect("swept");
    assert_eq!(
        reclaimed, 2,
        "two claims past any plausible lease survived the sweep"
    );
    assert_eq!(journal.claims().await.expect("read"), (0, 0));
    let _ = (stale, fresh);
}

/// The store's own clock. The reclaim takes the clock as an ARGUMENT precisely
/// so this test is expressible: one that cannot move time cannot tell a stale
/// claim from a fresh one, and a rule that cannot is indistinguishable from one
/// that discards everything.
fn now_ms() -> i64 {
    roost_worker::event_store::database::claims::now_ms()
}

/// `emit` is the primitive: the row and the retirement of its claim are ONE
/// transaction, because two would leave a row on disk with a live claim behind
/// it — and that is the ORDINARY path, not a crash, so it would leak on every
/// single event rather than rarely.
#[tokio::test]
async fn emitting_an_event_spends_its_claim_in_the_same_transaction() {
    let scratch = Scratch::new("claim-emit");
    let journal = journal_in(&scratch).await;
    let claim = journal
        .reserve(DurableEventKind::Opened, 512)
        .await
        .expect("a fresh outbox has room");
    assert_eq!(journal.claims().await.expect("read"), (1, 1));

    let row = journal
        .emit(claim, &opened(SESSION))
        .await
        .expect("emitted");
    assert_eq!(row.kind, "opened");
    assert_eq!(
        journal.claims().await.expect("read"),
        (0, 0),
        "the row is on disk and the claim is still held, so the same capacity is counted twice"
    );
    assert_eq!(journal.pending().await.expect("read").len(), 1);
    journal.acknowledge(row.client_seq).await.expect("acked");
    assert!(journal.pending().await.expect("read").is_empty());
}
