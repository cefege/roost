//! Every public store-plumbing mutation bumps `revision` exactly once, and one
//! that changes nothing bumps it not at all.
//!
//! `revision` is the only thing a host has to watch to know a repaint is owed
//! (`store.rs:64-65`), so a mutation that writes without bumping is a change the
//! user never sees — a toast that does not appear, a transfer that stalls at 99%.
//! The composite cases matter as much as the simple ones: a spawn settlement that
//! drops a placeholder AND raises a card is one user-visible event, and so is a
//! sign-out that empties six slices.
//!
//! The mutation experiment for this file is in the slice report: delete the
//! `store.note_change()` inside `toasts::add_toast` and
//! `toast_mutations_each_bump_the_revision_exactly_once` must fail.
//!

#![allow(clippy::unwrap_used, clippy::expect_used)]
use roost_client_core::store::Store;
use roost_client_core::store::mutations::{PairRequest, delete_pair_request, replace_workers};
use roost_client_core::store::optimistic_spawn::{
    SpawnSettlement, SupersededReason, begin_optimistic_spawn, settle_spawn_rejected,
};
use roost_client_core::store::root::{
    BrowserAccessState, captured_generation_is_current, clear_account_state_for_logout,
    invalidate_auth_resources, set_browser_access_state,
};
use roost_client_core::store::spotlight::{
    clear_spotlight, is_spotlit, set_spotlight_session_id, set_visible_pane_count,
};
use roost_client_core::store::toasts::{
    ToastId, ToastKind, ToastOptions, ToastSource, add_toast, dismiss_toast, expire_due_toasts,
    hold_toast_dismiss, release_toast_dismiss,
};
use roost_client_core::store::transfers::{
    NewTransfer, TRANSFER_STALL_AFTER_MS, TransferDirection, TransferState, add_transfer,
    mark_transfer_state, remove_transfer, set_transfer_progress, sweep_transfers,
};
use roost_client_core::store::ui::{
    SIDEBAR_WIDTH_MAX, SIDEBAR_WIDTH_MIN, SidebarView, clamp_sidebar_width, load_ui,
    set_sidebar_view, set_sidebar_width,
};
use roost_client_core::{ClientCore, MemoryKeyValueStore};

/// A session id of the shape the coordinator mints.
const SESSION_ONE: &str = "00000000-0000-4000-8000-00000000000a";
const SESSION_TWO: &str = "00000000-0000-4000-8000-00000000000b";

/// A client over the in-memory host.
fn client() -> ClientCore {
    ClientCore::in_memory("tab-plumbing")
}

/// Assert that `mutation` moved the revision exactly once.
///
/// The label is in the assertion message, so a failure names the mutation rather
/// than the helper.
fn assert_one_bump(core: &mut ClientCore, what: &str, mutation: impl FnOnce(&mut Store)) {
    let before = core.store().revision();
    mutation(core.store_mut());
    let after = core.store().revision();
    assert_eq!(
        after,
        before + 1,
        "{what} must bump the revision exactly once, and it moved {}",
        after - before
    );
}

/// Assert that `mutation` moved the revision not at all.
fn assert_no_bump(core: &mut ClientCore, what: &str, mutation: impl FnOnce(&mut Store)) {
    let before = core.store().revision();
    mutation(core.store_mut());
    let after = core.store().revision();
    assert_eq!(after, before, "{what} changed nothing and must not bump");
}

/// The id of a card raised by a Sync frame, which is what makes the frame
/// redeliverable onto the same card.
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

#[test]
fn toast_mutations_each_bump_the_revision_exactly_once() {
    let mut core = client();
    let id = sync_toast(41, SESSION_ONE);
    assert_one_bump(&mut core, "add_toast", |store| {
        add_toast(
            store,
            id.clone(),
            "opened",
            ToastKind::Ok,
            ToastOptions::plain(),
            1_000,
        );
    });
    assert_one_bump(&mut core, "add_toast replacing the same event", |store| {
        add_toast(
            store,
            id.clone(),
            "opened again",
            ToastKind::Ok,
            ToastOptions::plain(),
            1_010,
        );
    });
    assert_one_bump(&mut core, "hold_toast_dismiss", |store| {
        assert!(hold_toast_dismiss(store, &id, 1_020));
    });
    assert_one_bump(&mut core, "release_toast_dismiss", |store| {
        assert!(release_toast_dismiss(store, &id, 1_030));
    });
    assert_one_bump(&mut core, "dismiss_toast", |store| {
        assert!(dismiss_toast(store, &id));
    });
}

#[test]
fn a_mutation_that_changes_nothing_does_not_bump() {
    let mut core = client();
    let id = sync_toast(7, SESSION_ONE);
    add_toast(
        core.store_mut(),
        id.clone(),
        "opened",
        ToastKind::Ok,
        ToastOptions::plain(),
        0,
    );
    let absent = sync_toast(8, SESSION_TWO);
    assert_no_bump(&mut core, "dismissing a card that is not there", |store| {
        assert!(!dismiss_toast(store, &absent));
    });
    assert_no_bump(&mut core, "holding a card that is not there", |store| {
        assert!(!hold_toast_dismiss(store, &absent, 10));
    });
    assert_no_bump(&mut core, "expiring nothing", |store| {
        assert!(!expire_due_toasts(store, 10));
    });
    set_spotlight_session_id(core.store_mut(), Some(SESSION_ONE.to_owned()));
    assert_no_bump(
        &mut core,
        "spotlighting the session already spotlit",
        |store| {
            assert!(!set_spotlight_session_id(
                store,
                Some(SESSION_ONE.to_owned())
            ));
        },
    );
    assert_no_bump(
        &mut core,
        "removing a transfer that is not there",
        |store| {
            assert!(!remove_transfer(store, "nope"));
        },
    );
    assert_no_bump(
        &mut core,
        "dismissing a pair request that is not there",
        |store| {
            assert!(!delete_pair_request(store, "nope"));
        },
    );
    assert_no_bump(
        &mut core,
        "replacing the registry with what it holds",
        |store| {
            assert!(!replace_workers(store, std::collections::BTreeMap::new()));
        },
    );
}

#[test]
fn transfer_mutations_each_bump_the_revision_exactly_once() {
    let mut core = client();
    assert_one_bump(&mut core, "add_transfer", |store| {
        add_transfer(
            store,
            NewTransfer {
                id: "upload-1".to_owned(),
                name: "api-gateway.tgz".to_owned(),
                direction: TransferDirection::Up,
                bytes_total: 1_000,
                state: TransferState::Queued,
                preview_url: Some("blob:one".to_owned()),
                now_ms: 0,
            },
        );
    });
    assert_one_bump(&mut core, "set_transfer_progress", |store| {
        assert!(set_transfer_progress(store, "upload-1", 100, None, 100));
    });
    assert_one_bump(&mut core, "mark_transfer_state", |store| {
        assert!(mark_transfer_state(
            store,
            "upload-1",
            TransferState::Running,
            None,
            100
        ));
    });
    assert_one_bump(&mut core, "sweep_transfers stalling", |store| {
        assert!(sweep_transfers(store, 100 + TRANSFER_STALL_AFTER_MS));
    });
    assert_one_bump(&mut core, "mark_transfer_state settling", |store| {
        assert!(mark_transfer_state(
            store,
            "upload-1",
            TransferState::Done,
            None,
            20_000
        ));
    });
    assert_one_bump(&mut core, "sweep_transfers removing", |store| {
        assert!(sweep_transfers(store, 30_000));
    });
    assert_eq!(core.store().transfers.len(), 0);
}

#[test]
fn chrome_and_preference_mutations_each_bump_the_revision_exactly_once() {
    let mut core = client();
    let storage = MemoryKeyValueStore::default();
    assert_one_bump(&mut core, "set_spotlight_session_id", |store| {
        assert!(set_spotlight_session_id(
            store,
            Some(SESSION_ONE.to_owned())
        ));
    });
    assert!(is_spotlit(core.store(), SESSION_ONE));
    assert_one_bump(&mut core, "clear_spotlight", |store| {
        assert!(clear_spotlight(store));
    });
    assert_one_bump(&mut core, "set_visible_pane_count", |store| {
        assert!(set_visible_pane_count(store, 3));
    });
    assert_one_bump(&mut core, "set_sidebar_view", |store| {
        assert!(set_sidebar_view(store, &storage, SidebarView::Agents));
    });
    assert_one_bump(&mut core, "set_sidebar_width", |store| {
        assert!(set_sidebar_width(store, &storage, 320));
    });
    assert_one_bump(&mut core, "set_browser_access_state", |store| {
        assert!(set_browser_access_state(
            store,
            BrowserAccessState::Unauthorized
        ));
    });
    assert_one_bump(&mut core, "invalidate_auth_resources", |store| {
        assert!(invalidate_auth_resources(store) == 0);
    });
    assert_no_bump(
        &mut core,
        "load_ui over a store already holding the defaults",
        |store| {
            // Nothing is stored, and the defaults are already what the store holds: a
            // load that changes nothing must not repaint.
            assert!(!load_ui(store, &storage));
        },
    );
    // NOT wrapped in `assert_no_bump`: the first call in this closure DOES move
    // the width (300 -> 200), so the closure as a whole bumps once and the
    // outer helper's "changed nothing" reading was never true of it. What the
    // rule actually says is that the SECOND call changes nothing, because 199
    // and 0 both clamp to the same minimum — so the revision is sampled
    // inside, after the first write has already landed.
    let before_width = core.store().revision();
    set_sidebar_width(core.store_mut(), &storage, 0);
    assert_eq!(
        core.store().revision(),
        before_width + 1,
        "the first write moves the width off the default, so it is a change"
    );
    let after_width = core.store().revision();
    set_sidebar_width(core.store_mut(), &storage, SIDEBAR_WIDTH_MIN - 1);
    assert_eq!(
        core.store().revision(),
        after_width,
        "a width of 199 clamps to the same minimum 0 did, so it changed nothing \
         and must not bump"
    );
    // The same shape as the minimum case above, and for the same reason: the
    // first call in this closure DOES move the width, so wrapping the pair in
    // `assert_no_bump` made the helper measure the setup rather than the rule.
    // What the rule says is that the SECOND call changes nothing, because
    // `SIDEBAR_WIDTH_MAX + 1` clamps to the maximum the first call just wrote.
    set_sidebar_width(core.store_mut(), &storage, SIDEBAR_WIDTH_MAX);
    let after_max = core.store().revision();
    set_sidebar_width(core.store_mut(), &storage, SIDEBAR_WIDTH_MAX + 1);
    assert_eq!(
        core.store().revision(),
        after_max,
        "a width one past the maximum clamps to the maximum just written, so it \
         changed nothing and must not bump"
    );
    assert_eq!(core.store().ui.sidebar_width, SIDEBAR_WIDTH_MAX);
    assert_eq!(clamp_sidebar_width(0), SIDEBAR_WIDTH_MIN);
    assert_eq!(clamp_sidebar_width(9_999), SIDEBAR_WIDTH_MAX);
}

#[test]
fn the_credential_boundary_is_one_mutation_and_one_bump() {
    let mut core = client();
    let store = core.store_mut();
    add_toast(
        store,
        sync_toast(1, SESSION_ONE),
        "left over",
        ToastKind::Ok,
        ToastOptions::plain(),
        0,
    );
    add_transfer(
        store,
        NewTransfer {
            id: "upload-1".to_owned(),
            name: "one.tgz".to_owned(),
            direction: TransferDirection::Up,
            bytes_total: 10,
            state: TransferState::Running,
            preview_url: None,
            now_ms: 0,
        },
    );
    let ticket = begin_optimistic_spawn(store, SESSION_ONE, "f".repeat(64), "/x", None, 0)
        .expect("a uuid is a usable session id");
    assert!(store.spawns.is_client_only(SESSION_ONE));
    // The fixture writes the field directly: the INSERT is the Sync projector's
    // (`projector.ts` folds a pair request in), and `mutations.ts` only ever
    // deletes one. What this test is about is the boundary that empties it.
    store.pair_requests.insert(
        "ephemeral-1".to_owned(),
        PairRequest {
            ephemeral_id: "ephemeral-1".to_owned(),
            label: "laptop".to_owned(),
            created_at_ms: 1,
            user_agent: "ua".to_owned(),
            client_browser: "b".to_owned(),
            client_os: "linux".to_owned(),
            client_device_type: "laptop".to_owned(),
            source_ip: "203.0.113.1".to_owned(),
            country_code: "SE".to_owned(),
            region: "Stockholm".to_owned(),
            city: "Stockholm".to_owned(),
            edge_identity_provider: String::new(),
            edge_identity: String::new(),
            edge_identity_verified: false,
            expires_at_ms: 60_000,
        },
    );
    set_browser_access_state(store, BrowserAccessState::Authorized);
    let generation = store.auth_generation;

    let before = core.store().revision();
    clear_account_state_for_logout(core.store_mut());
    assert_eq!(
        core.store().revision(),
        before + 1,
        "a sign-out is one user-visible event however many records it empties"
    );
    let store = core.store();
    assert!(
        store.toasts.is_empty(),
        "a card can quote a machine the new account cannot see"
    );
    assert!(
        store.transfers.is_empty(),
        "a card names a file and a path from the old account"
    );
    assert!(store.spawns.is_empty());
    assert!(store.pair_requests.is_empty());
    assert!(store.pending_closes.is_empty());
    assert_eq!(store.browser_access_state, BrowserAccessState::Checking);
    assert!(
        !captured_generation_is_current(store, generation),
        "work captured under the old credential may no longer write"
    );
    // The ticket the old scope was holding is now unanswerable, not misapplied.
    let after = core.store().revision();
    assert_eq!(
        settle_spawn_rejected(core.store_mut(), &ticket, "late", 9_000),
        SpawnSettlement::Superseded(SupersededReason::NeverExisted)
    );
    assert_eq!(
        core.store().revision(),
        after,
        "an answer from a scope that was reset must not land in the new one"
    );
    assert!(core.store().toasts.is_empty());
}
