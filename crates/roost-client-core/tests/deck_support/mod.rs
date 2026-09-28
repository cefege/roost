//! Builders the deck tests share: session ids, open session rows, pane trees,
//! and a core over one storage a second core can reload from.
#![allow(dead_code)]

use std::rc::Rc;

use roost_client_core::deck::{DeckFolder, DeckIntent};
use roost_client_core::store::layout::{PaneLayout, PaneLeaf, PaneNode, PaneSplit};
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, WorkerFp,
};
use roost_client_core::{ClientCore, ClientEvent, MemoryClock, MemoryKeyValueStore};
use roost_protocol::layout::document::LayoutDirection;

pub const MACHINE: &str = "00000000000000000000000000000000000000000000000000000000000000ff";
pub const FOLDER: &str = "folder-a";

/// The session id numbered `n`, in the uuid shape the coordinator mints.
pub fn sid(n: u8) -> String {
    format!("00000000-0000-4000-8000-0000000000{n:02x}")
}

/// An open shell row numbered `n`.
pub fn open_session(n: u8) -> Session {
    Session {
        id: SessionId::try_from(sid(n)).expect("a uuid"),
        worker_fp: WorkerFp::try_from(MACHINE.to_owned()).expect("a fingerprint"),
        channel: ChannelId::try_from(7_i64).expect("a channel"),
        kind: SessionKind::Shell,
        cwd: "/home/dev".to_owned(),
        spawn_cwd: Some("/home/dev".to_owned()),
        workspace_id: None,
        status: SessionStatus::Open,
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

/// Put open rows for `ns` into the core's session plane.
pub fn open_sessions(core: &mut ClientCore, ns: &[u8]) {
    let mut map = SessionMap::new();
    for n in ns {
        let row = open_session(*n);
        map.insert(row.id.clone(), row);
    }
    core.store_mut().sessions.apply_snapshot(map);
}

/// The folder with `ns` live, oldest first.
pub fn folder(ns: &[u8]) -> DeckFolder {
    DeckFolder {
        folder_key: FOLDER.to_owned(),
        live_session_ids: ns.iter().map(|n| sid(*n)).collect(),
    }
}

pub fn leaf(pane_id: &str, tabs: &[u8], selected: Option<u8>) -> PaneNode {
    PaneNode::Leaf(PaneLeaf {
        pane_id: pane_id.to_owned(),
        tabs: tabs.iter().map(|n| sid(*n)).collect(),
        selected_tab: selected.map(sid).unwrap_or_default(),
    })
}

pub fn row_split(id: &str, first: PaneNode, second: PaneNode) -> PaneNode {
    PaneNode::Split(PaneSplit {
        id: id.to_owned(),
        direction: LayoutDirection::Row,
        ratio: 0.5,
        a: Box::new(first),
        b: Box::new(second),
    })
}

pub fn layout(root: PaneNode, focused: &str) -> PaneLayout {
    PaneLayout {
        root,
        focused_pane_id: focused.to_owned(),
    }
}

/// A core whose storage outlives it, so a second core can reload it.
pub fn core_over(storage: &Rc<MemoryKeyValueStore>) -> ClientCore {
    ClientCore::new(
        Rc::new(MemoryClock::starting_at(1_000)),
        storage.clone(),
        "tab-deck",
    )
}

pub fn core() -> ClientCore {
    core_over(&Rc::new(MemoryKeyValueStore::new()))
}

pub fn deck(core: &mut ClientCore, intent: DeckIntent) {
    let effects = core.handle(ClientEvent::Deck(intent));
    assert!(effects.is_empty(), "a deck intent owes the host no effect");
}

/// Commit `arrangement` straight into the records, the way an applied layout
/// document does, so a test can start from a split without a drag.
pub fn stored(core: &mut ClientCore, arrangement: PaneLayout) {
    let (records, _) = core.store_mut().deck.layout_state();
    records.commit(FOLDER, arrangement);
}

pub fn stored_layout(core: &ClientCore) -> PaneLayout {
    core.store()
        .deck
        .records()
        .stored(FOLDER)
        .cloned()
        .expect("a stored layout")
}

pub fn navigated(core: &ClientCore) -> Option<String> {
    core.store()
        .deck
        .navigation()
        .map(|navigation| navigation.path.clone())
}
