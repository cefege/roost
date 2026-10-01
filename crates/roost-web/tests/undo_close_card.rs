//! The undo card's Undo, and the one it has to reach to put a tab back.
//!
//! A close takes a session out of TWO places: the live list the selectors read,
//! and the pane arrangement the deck paints. The queue's `undo_one` gives back
//! only the first. This card is the single undo affordance for BOTH close paths
//! — a pane tab's ✕ and a sidebar row's ✕ — and it used to call that queue
//! function directly, so the tab came back re-derived by `reconcile` as an
//! orphan at the end of the rail instead of in the position it was closed from.
//! It now raises the deck's `UndoClose`, which restores the recorded tiling.
//!
//! The observable is the ARRANGEMENT the deck resolves for the folder, read
//! through the same core and the same selectors the deck paints from, plus the
//! STORE RECORD that survives a reload. Target-independent: a real
//! `ClientCore` over real storage, driven by the same `ClientEvent`s the card's
//! button raises.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;

use roost_client_core::deck::{DeckFolder, DeckIntent};
use roost_client_core::store::layout::all_leaves;
use roost_client_core::store::pending_close::{CloseLabels, is_pending_close, undo_one};
use roost_client_core::store::selectors::{
    live_session_ids_for_folder, session_by_id, session_folder_key,
};
use roost_client_core::store::{Session, SessionMap};
use roost_client_core::{ClientCore, ClientEvent, MemoryClock, MemoryKeyValueStore};
use roost_web::platform::worker_paths::BrowserWorkerPaths;

/// The rail: one pane, four tabs, closed from the middle — the shape a pane
/// tab's ✕ produces in a narrow desktop strip.
const TABS: [u8; 4] = [1, 2, 3, 4];
const CLOSED: u8 = 3;
const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";

fn sid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
}

/// The folder the deck commits into, with the live set the REAL selector
/// derives. Hand-building that list asserts against a membership no production
/// call ever sees: the selector filters a pending close out, which is the whole
/// reason a close hides its tab before the kill round-trips.
fn folder(core: &ClientCore) -> DeckFolder {
    let session = session_by_id(core.store(), &sid(1)).expect("session 1 is open");
    let folder_key = session_folder_key(core.store(), &BrowserWorkerPaths, session);
    DeckFolder {
        live_session_ids: live_session_ids_for_folder(
            core.store(),
            &BrowserWorkerPaths,
            &folder_key,
        ),
        folder_key,
    }
}

fn core_over(storage: &Rc<MemoryKeyValueStore>) -> ClientCore {
    ClientCore::new(
        Rc::new(MemoryClock::new()),
        Rc::clone(storage) as Rc<dyn roost_client_core::KeyValueStore>,
        "undo-card",
        &roost_client_core::store::prefs::PrefDefaults::default(),
    )
}

fn core() -> ClientCore {
    core_over(&Rc::new(MemoryKeyValueStore::new()))
}

/// An open shell row numbered `n`.
fn open_session(n: u8) -> Session {
    Session {
        id: roost_protocol::wire::SessionId::try_from(sid(n)).expect("a uuid"),
        worker_fp: roost_protocol::wire::WorkerFp::try_from(MACHINE.to_owned())
            .expect("a fingerprint"),
        channel: roost_protocol::wire::ChannelId::try_from(7_i64).expect("a channel"),
        kind: roost_protocol::wire::SessionKind::Shell,
        cwd: "/home/dev".to_owned(),
        spawn_cwd: Some("/home/dev".to_owned()),
        workspace_id: None,
        status: roost_protocol::wire::SessionStatus::Open,
        created_at: i64::from(n),
        closed_at: None,
        custom_title: None,
        git_branch: None,
        git_remote: None,
        pr_number: None,
        pr_state: None,
        pr_checks: None,
        pr_url: None,
        ports: None,
    }
}

fn seed(core: &mut ClientCore) {
    let mut sessions = SessionMap::new();
    for n in TABS {
        let row = open_session(n);
        sessions.insert(row.id.clone(), row);
    }
    core.store_mut().sessions.apply_snapshot(sessions);
    let folder = folder(core);
    core.handle(ClientEvent::Deck(DeckIntent::Observed {
        folder: Some(folder),
        followed_session_id: Some(sid(1)),
        compact: false,
        visible_pane_count: 1,
    }));
}

/// What the deck PAINTS for the folder: the stored arrangement folded against
/// the live set, which is the tab list a reader sees in the strip.
fn rail(core: &ClientCore) -> Vec<String> {
    all_leaves(&core.store().deck.resolve_layout(&folder(core)).root)
        .into_iter()
        .flat_map(|leaf| leaf.tabs.clone())
        .collect()
}

fn close_middle(core: &mut ClientCore) {
    let folder = folder(core);
    core.handle(ClientEvent::Deck(DeckIntent::CloseTab {
        folder: Some(folder),
        session_id: sid(CLOSED),
        active_session_id: Some(sid(1)),
        labels: CloseLabels::default(),
    }));
}

/// THE REGRESSION. A tab is closed out of a four-tab rail, which is what a pane
/// tab's ✕ and a sidebar row's ✕ both raise a card for, and the card's Undo
/// runs. The tab must return BETWEEN its neighbours.
///
/// The failure this pins is silent. The session comes back — it is no longer
/// pending, so the status bar's count is right and the sidebar lists the folder
/// — while the tab is only re-derived by `reconcile`, which appends it at the
/// end of the rail. Every count still looks right, and the tab the reader just
/// closed is not where they left it.
#[test]
fn the_cards_undo_puts_a_closed_tab_back_where_it_was() {
    let mut core = core();
    seed(&mut core);
    assert_eq!(rail(&core).len(), 4, "the rail starts full");

    close_middle(&mut core);
    assert!(is_pending_close(core.store(), &sid(CLOSED)));
    assert_eq!(
        rail(&core),
        vec![sid(1), sid(2), sid(4)],
        "the closed tab leaves the rail at once"
    );

    // What the card's Undo button raises.
    core.handle(ClientEvent::Deck(DeckIntent::UndoClose {
        session_id: sid(CLOSED),
    }));

    assert!(!is_pending_close(core.store(), &sid(CLOSED)));
    assert_eq!(
        rail(&core),
        vec![sid(1), sid(2), sid(3), sid(4)],
        "the tab returns between its neighbours, not appended at the end of \
         the rail — a re-derived arrangement is a tab the reader cannot find"
    );
}

/// WHY IT IS THE DECK'S UNDO AND NOT THE QUEUE'S. The queue's `undo_one` is a
/// real, working restore — of the LIVE LIST. It is not a restore of the pane
/// arrangement, which only the deck recorded, so calling it here leaves the tab
/// in the rail at the END. Held apart from the fix so a future reader can see
/// which half each call owns, and cannot mistake this for a broken `undo_one`.
#[test]
fn the_queues_undo_alone_restores_the_row_but_not_the_position() {
    let mut core = core();
    seed(&mut core);
    close_middle(&mut core);

    undo_one(core.store_mut(), &sid(CLOSED));

    assert!(!is_pending_close(core.store(), &sid(CLOSED)));
    assert_eq!(
        rail(&core),
        vec![sid(1), sid(2), sid(4), sid(3)],
        "the row is live again but the tab is appended: the half of the \
         restore the queue does not own, and the reason the card asks the deck"
    );
}

/// The restore is a STORE write, not a local one: a reader who undoes, reloads
/// and comes back must find the tab they put back, not the arrangement the
/// close left behind. A restore that only lived in the store would repaint
/// correctly and then vanish on the next reload.
#[test]
fn the_undone_arrangement_survives_a_reload() {
    let storage = Rc::new(MemoryKeyValueStore::new());
    let mut core = core_over(&storage);
    seed(&mut core);
    let folder_key = folder(&core).folder_key;
    close_middle(&mut core);
    core.handle(ClientEvent::Deck(DeckIntent::UndoClose {
        session_id: sid(CLOSED),
    }));

    let reloaded = core_over(&storage);
    // The reloaded core has no session rows yet — a real reader gets those from
    // the coordinator's snapshot — so the arrangement is read off the record
    // the undo persisted, which is the thing a reload has to have kept.
    let restored = reloaded
        .store()
        .deck
        .records()
        .stored(&folder_key)
        .expect("the folder's arrangement was persisted");
    assert_eq!(
        all_leaves(&restored.root)
            .into_iter()
            .flat_map(|leaf| leaf.tabs.clone())
            .collect::<Vec<_>>(),
        vec![sid(1), sid(2), sid(3), sid(4)],
        "the arrangement the undo committed is the one a reload reads back"
    );
}
