//! Toasts: one card per event, and every card dismisses.
//!
//! Three ways a toast layer goes wrong that a user reports within seconds — a
//! card that fires twice for one event, a card that never goes away, and a card
//! that burns its window while the user is reading it. All three are decided in
//! `store::toasts`, not at the call sites.
//!
//! The mutation experiment for this file is in the slice report: make
//! `add_toast` push onto a `Vec` instead of a map keyed by `ToastId`, and
//! `the_same_event_cannot_produce_two_toasts` must fail.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::ClientCore;
use roost_client_core::store::Store;
use roost_client_core::store::toasts::{
    Toast, ToastId, ToastIntent, ToastKind, ToastOptions, ToastSource, add_toast, dismiss_toast,
    drop_toasts_for_session, expire_due_toasts, hold_toast_dismiss, release_toast_dismiss,
    take_toast_action,
};

/// Session ids of the shape the coordinator mints.
const SESSION_ONE: &str = "00000000-0000-4000-8000-00000000000a";
const SESSION_TWO: &str = "00000000-0000-4000-8000-00000000000b";

/// A client over the in-memory host.
fn client() -> ClientCore {
    ClientCore::in_memory("tab-toasts")
}

/// The id of a card raised by a Sync frame.
fn sync_toast(delivery_seq: u64, subject: &str) -> ToastId {
    ToastId::new(
        ToastSource::Sync {
            domain: "sessions",
            kind: "session_opened",
            delivery_seq,
        },
        subject,
    )
}

/// The only card, asserting there is exactly one.
fn only_toast(store: &Store) -> &Toast {
    let cards: Vec<&Toast> = store.toasts.toasts().collect();
    assert_eq!(cards.len(), 1, "expected exactly one card");
    cards[0]
}

#[test]
fn the_same_event_cannot_produce_two_toasts() {
    let mut core = client();
    let id = sync_toast(41, SESSION_ONE);
    for msg in ["first delivery", "second delivery", "third delivery"] {
        add_toast(
            core.store_mut(),
            id.clone(),
            msg,
            ToastKind::Ok,
            ToastOptions::plain(),
            1_000,
        );
    }
    let store = core.store();
    assert_eq!(store.toasts.len(), 1, "one event, one card");
    assert_eq!(only_toast(store).msg, "third delivery");
    // A DIFFERENT event about the same session is a different card: the identity
    // is the event, not the subject.
    add_toast(
        core.store_mut(),
        sync_toast(42, SESSION_ONE),
        "closed",
        ToastKind::Warn,
        ToastOptions::plain(),
        1_000,
    );
    assert_eq!(core.store().toasts.len(), 2);
}

#[test]
fn a_toast_always_dismisses() {
    let mut core = client();
    let id = sync_toast(3, SESSION_ONE);
    add_toast(
        core.store_mut(),
        id.clone(),
        "needs your input",
        ToastKind::Warn,
        ToastOptions::plain(),
        1_000,
    );
    assert_eq!(
        only_toast(core.store()).expires_at_ms.unwrap_or_default(),
        6_000,
        "a warn card lives five seconds from the instant it was raised"
    );
    // Nothing is due yet.
    assert!(!expire_due_toasts(core.store_mut(), 5_999));
    assert_eq!(core.store().toasts.len(), 1);
    // Held: the reader is on the card, so the clock does not run.
    assert!(hold_toast_dismiss(core.store_mut(), &id, 5_990));
    assert!(!expire_due_toasts(core.store_mut(), 60_000));
    assert_eq!(core.store().toasts.len(), 1, "a held card is not retired");
    // Released with ten milliseconds left, so it dies ten milliseconds later and
    // not before.
    assert!(release_toast_dismiss(core.store_mut(), &id, 5_990));
    assert!(!expire_due_toasts(core.store_mut(), 5_999));
    assert!(expire_due_toasts(core.store_mut(), 6_000));
    assert_eq!(core.store().toasts.len(), 0);
}

#[test]
fn a_card_with_no_window_stays_until_it_is_dismissed_by_hand() {
    let mut core = client();
    let id = sync_toast(9, SESSION_ONE);
    add_toast(
        core.store_mut(),
        id.clone(),
        "copy this before it goes",
        ToastKind::Err,
        ToastOptions::with_ttl(None),
        1_000,
    );
    assert!(!expire_due_toasts(core.store_mut(), 1_000_000));
    assert!(!hold_toast_dismiss(core.store_mut(), &id, 1_000_000));
    assert_eq!(core.store().toasts.len(), 1);
    assert!(dismiss_toast(core.store_mut(), &id));
    assert_eq!(core.store().toasts.len(), 0);
}

#[test]
fn taking_a_cards_button_removes_the_card_and_returns_the_intent() {
    let mut core = client();
    let id = sync_toast(11, SESSION_ONE);
    add_toast(
        core.store_mut(),
        id.clone(),
        "vim needs you",
        ToastKind::Warn,
        ToastOptions::plain().with_action("View", SESSION_ONE),
        1_000,
    );
    let action = take_toast_action(core.store_mut(), &id).expect("the card offers a button");
    assert_eq!(action.label, "View");
    assert_eq!(
        action.intent,
        ToastIntent::RevealSession {
            session_id: SESSION_ONE.to_owned()
        }
    );
    assert_eq!(
        core.store().toasts.len(),
        0,
        "the button is taken and the card goes with it, so a reveal cannot leave a card behind"
    );
    assert!(take_toast_action(core.store_mut(), &id).is_none());
}

#[test]
fn a_cards_session_going_away_takes_its_cards_with_it() {
    let mut core = client();
    add_toast(
        core.store_mut(),
        sync_toast(1, SESSION_ONE),
        "about one",
        ToastKind::Ok,
        ToastOptions::plain(),
        0,
    );
    add_toast(
        core.store_mut(),
        sync_toast(2, SESSION_TWO),
        "about two",
        ToastKind::Ok,
        ToastOptions::plain(),
        0,
    );
    assert!(drop_toasts_for_session(core.store_mut(), SESSION_ONE));
    assert_eq!(core.store().toasts.len(), 1);
    assert!(!drop_toasts_for_session(core.store_mut(), SESSION_ONE));
}

#[test]
fn the_stack_reads_in_raise_order_not_id_order() {
    // Agent cards are keyed by session id. Id order would put the card for
    // session `…0a` ABOVE an older card for `…0b`, so the newest card would land
    // mid-stack instead of at the edge the dock anchors to.
    let mut core = client();
    let agent = |session: &str| ToastId::new(ToastSource::Host { name: "agent" }, session);
    for (session, msg) in [(SESSION_TWO, "older"), (SESSION_ONE, "newer")] {
        add_toast(
            core.store_mut(),
            agent(session),
            msg,
            ToastKind::Ok,
            ToastOptions::plain(),
            0,
        );
    }
    let order = |core: &ClientCore| -> Vec<String> {
        core.store()
            .toasts
            .toasts()
            .map(|toast| toast.msg.clone())
            .collect()
    };
    assert_eq!(order(&core), ["older", "newer"]);
    // A redelivery replaces the card in place rather than moving it to the end.
    add_toast(
        core.store_mut(),
        agent(SESSION_TWO),
        "older, updated",
        ToastKind::Ok,
        ToastOptions::plain(),
        0,
    );
    assert_eq!(order(&core), ["older, updated", "newer"]);
    // A card raised after a dismissal still goes to the end.
    assert!(dismiss_toast(core.store_mut(), &agent(SESSION_TWO)));
    add_toast(
        core.store_mut(),
        agent(SESSION_TWO),
        "newest",
        ToastKind::Ok,
        ToastOptions::plain(),
        0,
    );
    assert_eq!(order(&core), ["newer", "newest"]);
}
