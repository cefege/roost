//! The deck's per-render model: the order its open sessions are mounted in,
//! which is the slots' DOM order and so the keyboard Tab order and the order a
//! reader walking the panes meets them. Pins
//! `components::deck::terminal_deck_model::deck_frame` (v2
//! `apps/web/src/components/deck/terminal-deck-model.ts` `mountedSessionIds`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;

use roost_client_core::ClientCore;
use roost_client_core::deck::DeckSize;
use roost_client_core::store::{
    ChannelId, Session, SessionId, SessionKind, SessionMap, SessionStatus, WorkerFp, WorkspaceId,
};
use roost_web::components::deck::terminal_deck_model::{DeckInputs, deck_frame};
use roost_web::platform::worker_paths::BrowserWorkerPaths;

const FOLDER: &str = "/Users/you/roost";
// The OLDER session has the LARGER id, so id order and creation order differ.
const OLDER: &str = "f0000000-0000-4000-8000-000000000001";
const NEWER: &str = "10000000-0000-4000-8000-000000000002";
const CLOSED: &str = "00000000-0000-4000-8000-000000000003";

fn session(id: &str, channel: i64, created_at: i64, status: SessionStatus) -> Session {
    Session {
        id: SessionId::try_from(id.to_owned()).unwrap(),
        worker_fp: WorkerFp::try_from("aa".repeat(32)).unwrap(),
        channel: ChannelId::try_from(channel).unwrap(),
        kind: SessionKind::Shell,
        cwd: FOLDER.into(),
        spawn_cwd: Some(FOLDER.into()),
        workspace_id: Some(
            WorkspaceId::try_from("00000000-0000-4000-8000-0000000000aa".to_owned()).unwrap(),
        ),
        status,
        created_at,
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

#[test]
fn open_sessions_are_mounted_oldest_first_not_in_id_order() {
    let mut core = ClientCore::in_memory("tab-deck-model");
    let mut map = SessionMap::new();
    for row in [
        session(NEWER, 2, 2_000, SessionStatus::Open),
        session(OLDER, 1, 1_000, SessionStatus::Open),
        session(CLOSED, 3, 500, SessionStatus::Closed),
    ] {
        map.insert(row.id.clone(), row);
    }
    core.store_mut().sessions.apply_snapshot(map);
    let ratios = BTreeMap::new();
    let frame = deck_frame(
        core.store(),
        &BrowserWorkerPaths,
        &DeckInputs {
            active_session_id: Some(OLDER),
            surface_visible: true,
            retained_session_id: None,
            compact: false,
            size: DeckSize {
                w: 1200.0,
                h: 800.0,
            },
            desktop_strip_height: 32.0,
            drag_ratios: &ratios,
            swipe_neighbor_id: None,
        },
    );
    assert_eq!(
        frame.open_session_ids,
        vec![OLDER.to_owned(), NEWER.to_owned()]
    );
}
