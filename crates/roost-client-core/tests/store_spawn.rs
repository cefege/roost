//! Optimistic spawn: a late answer from a superseded attempt must change nothing.
//!
//! A spawn has three outcomes, and the third is the one a naive port misses. It
//! can be admitted, it can be rejected, and it can be SUPERSEDED — the user
//! respawned, or closed the pending tab, or the answer arrived after a newer
//! attempt had already begun. The third is decided in the store, by the attempt
//! the ledger minted, and the call sites never compare anything.
//!
//! The mutation experiment for this file is in the slice report: in
//! `settle_spawn_rejected`, delete the `relevance` guard at the top, and
//! `a_superseded_spawn_answer_does_not_roll_back_the_newer_attempt` must fail.

use roost_client_core::ClientCore;
use roost_client_core::store::Store;
use roost_client_core::store::optimistic_spawn::{
    SpawnRefusal, SpawnSettlement, SupersededReason, abort_optimistic_spawn,
    begin_optimistic_spawn, reconcile_spawn, respawn_optimistic_spawn, settle_spawn_admitted,
    settle_spawn_rejected,
};
use roost_client_core::store::pending_close::{
    CloseLabels, UNDO_WINDOW_MS, is_pending_close, schedule_close, sweep_pending_closes, undo_one,
};
use roost_client_core::store::toasts::{Toast, ToastKind};

/// Session ids of the shape the coordinator mints.
const SESSION_ONE: &str = "00000000-0000-4000-8000-00000000000a";
const SESSION_TWO: &str = "00000000-0000-4000-8000-00000000000b";

/// A fingerprint-shaped machine id, which is all the anchor needs.
const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

/// The machine every spawn in this file anchors to.
fn machine() -> String {
    MACHINE.to_owned()
}

/// The only card, asserting there is exactly one.
fn only_toast(store: &Store) -> &Toast {
    let cards: Vec<&Toast> = store.toasts.toasts().collect();
    assert_eq!(cards.len(), 1, "expected exactly one card");
    cards[0]
}

/// Assert that `mutation` moved the revision exactly once.
fn assert_one_bump(core: &mut ClientCore, what: &str, mutation: impl FnOnce(&mut Store)) {
    let before = core.store().revision();
    mutation(core.store_mut());
    assert_eq!(
        core.store().revision(),
        before + 1,
        "{what} must bump the revision exactly once"
    );
}

/// A client over the in-memory host.
fn client() -> ClientCore {
    ClientCore::in_memory("tab-spawn")
}

#[test]
fn a_superseded_spawn_answer_does_not_roll_back_the_newer_attempt() {
    let mut core = client();
    let first = begin_optimistic_spawn(
        core.store_mut(),
        SESSION_ONE,
        machine(),
        "/home/dev/api",
        Some("11111111-1111-4111-8111-111111111111".to_owned()),
        1_000,
    )
    .expect("a uuid is a usable session id");
    // The user respawns in place while the first request is still in flight.
    let second = respawn_optimistic_spawn(core.store_mut(), &first, 1_100).expect("respawn");
    assert_eq!(second.session_id, first.session_id);
    assert!(second.attempt > first.attempt);

    // The FIRST request's failure lands late. It must not remove the placeholder
    // the second attempt is holding, and it must not raise a card.
    let before = core.store().revision();
    let settlement = settle_spawn_rejected(core.store_mut(), &first, "worker refused", 1_200);
    assert_eq!(
        settlement,
        SpawnSettlement::Superseded(SupersededReason::ReplacedByNewerAttempt)
    );
    assert_eq!(
        core.store().revision(),
        before,
        "a refused answer changes nothing at all"
    );
    assert!(
        core.store().spawns.is_client_only(SESSION_ONE),
        "the newer attempt's placeholder is still there"
    );
    assert_eq!(
        core.store().spawns.ticket_for(SESSION_ONE),
        Some(second.clone())
    );
    assert!(
        core.store().toasts.is_empty(),
        "a failure the user already superseded is not a failure they are waiting to hear about"
    );

    // The SECOND attempt's answer is the one that lands.
    assert_eq!(
        settle_spawn_admitted(core.store_mut(), &second),
        SpawnSettlement::Applied
    );
    assert!(!core.store().spawns.is_pending(SESSION_ONE));
    assert!(core.store().spawns.is_client_only(SESSION_ONE));
    assert!(reconcile_spawn(core.store_mut(), SESSION_ONE));
    assert!(!reconcile_spawn(core.store_mut(), SESSION_ONE));
}

#[test]
fn a_rejection_after_admission_changes_nothing() {
    let mut core = client();
    let ticket = begin_optimistic_spawn(
        core.store_mut(),
        SESSION_ONE,
        machine(),
        "/home/dev",
        None,
        1_000,
    )
    .expect("a uuid is a usable session id");
    assert_eq!(
        settle_spawn_admitted(core.store_mut(), &ticket),
        SpawnSettlement::Applied
    );
    let before = core.store().revision();
    assert_eq!(
        settle_spawn_rejected(core.store_mut(), &ticket, "too late", 1_100),
        SpawnSettlement::Superseded(SupersededReason::AlreadySettled)
    );
    assert_eq!(core.store().revision(), before);
    assert!(
        core.store().spawns.is_client_only(SESSION_ONE),
        "a terminal that was admitted must not be deleted by a refusal that arrives after it"
    );
    assert!(core.store().toasts.is_empty());
}

#[test]
fn a_retracted_spawn_answer_is_silent() {
    let mut core = client();
    let ticket = begin_optimistic_spawn(
        core.store_mut(),
        SESSION_ONE,
        machine(),
        "/home/dev",
        None,
        1_000,
    )
    .expect("a uuid is a usable session id");
    assert!(abort_optimistic_spawn(core.store_mut(), &ticket));
    let before = core.store().revision();
    assert_eq!(
        settle_spawn_rejected(core.store_mut(), &ticket, "gone", 1_100),
        SpawnSettlement::Superseded(SupersededReason::Retracted)
    );
    assert_eq!(core.store().revision(), before);
    assert!(core.store().toasts.is_empty());
    assert!(
        !abort_optimistic_spawn(core.store_mut(), &ticket),
        "one retraction, once"
    );
}

#[test]
fn a_spawn_whose_own_attempt_fails_raises_exactly_one_card() {
    let mut core = client();
    let ticket = begin_optimistic_spawn(
        core.store_mut(),
        SESSION_ONE,
        machine(),
        "/home/dev",
        None,
        1_000,
    )
    .expect("a uuid is a usable session id");
    let before = core.store().revision();
    assert_eq!(
        settle_spawn_rejected(core.store_mut(), &ticket, "no keeper", 1_100),
        SpawnSettlement::Applied
    );
    assert_eq!(
        core.store().revision(),
        before + 1,
        "dropping the placeholder and raising the card are one user-visible event"
    );
    assert!(!core.store().spawns.is_client_only(SESSION_ONE));
    let card = only_toast(core.store());
    assert_eq!(card.msg, "New terminal failed: no keeper");
    assert_eq!(card.kind, ToastKind::Err);
    // A second refusal for the same attempt is refused, and raises nothing.
    let after = core.store().revision();
    assert_eq!(
        settle_spawn_rejected(core.store_mut(), &ticket, "no keeper", 1_200),
        SpawnSettlement::Superseded(SupersededReason::AlreadySettled)
    );
    assert_eq!(core.store().revision(), after, "one card, one settlement");
    assert_eq!(core.store().toasts.len(), 1);
}

#[test]
fn a_session_id_that_is_not_a_uuid_is_refused_rather_than_held_forever() {
    let mut core = client();
    let before = core.store().revision();
    // The VARIANT, not `is_err()`. `begin_optimistic_spawn` has two refusals,
    // and `is_err()` would be satisfied by either — a test named for the shape
    // check that cannot tell the shape check from the other rule.
    assert!(matches!(
        begin_optimistic_spawn(core.store_mut(), "not-a-uuid", machine(), "/x", None, 0),
        Err(SpawnRefusal::NotAUuid { .. })
    ));
    assert_eq!(core.store().revision(), before);
    assert!(core.store().spawns.is_empty());
}

#[test]
fn a_pending_close_is_hidden_from_the_live_list_and_an_undo_restores_it() {
    let mut core = client();
    let labels = CloseLabels {
        terminal_name: "vim api".to_owned(),
        folder: "api".to_owned(),
        server: "workstation".to_owned(),
    };
    assert_one_bump(&mut core, "schedule_close", |store| {
        assert!(schedule_close(store, SESSION_ONE, labels.clone(), 1_000));
    });
    assert!(is_pending_close(core.store(), SESSION_ONE));
    // Nothing is due yet.
    assert!(sweep_pending_closes(core.store_mut(), 1_000 + UNDO_WINDOW_MS - 1).is_empty());
    let due = sweep_pending_closes(core.store_mut(), 1_000 + UNDO_WINDOW_MS);
    assert_eq!(
        due,
        vec![SESSION_ONE.to_owned()],
        "the sweep owes the host its kill"
    );
    assert!(!is_pending_close(core.store(), SESSION_ONE));

    let other = CloseLabels {
        terminal_name: "tail".to_owned(),
        folder: "api".to_owned(),
        server: "workstation".to_owned(),
    };
    schedule_close(core.store_mut(), SESSION_TWO, other.clone(), 2_000);
    assert_eq!(
        undo_one(core.store_mut(), SESSION_TWO),
        Some(other),
        "undo hands the labels back so the host can re-commit what the close undid"
    );
    assert!(undo_one(core.store_mut(), SESSION_TWO).is_none());
}
