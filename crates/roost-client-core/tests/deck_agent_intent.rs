//! Agent deck tabs select their route and use the undoable close path.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod deck_support;

use deck_support::*;
use roost_client_core::deck::{DeckFolder, DeckIntent};
use roost_client_core::store::layout::find_leaf_of_tab;
use roost_client_core::store::pending_close::{CloseLabels, is_pending_close};
use roost_protocol::wire::agent_chat::{AgentRunState, ConversationSummary};

#[test]
fn agent_tab_selects_agent_route_and_undo_restores_its_tab() {
    let mut core = core();
    let conversation = ConversationSummary {
        id: "abc".to_owned(),
        title: "Agent conversation".to_owned(),
        worker_fp: MACHINE.to_owned(),
        worker_label: "dev".to_owned(),
        cwd: "/home/dev".to_owned(),
        mode: "normal".into(),
        parent_id: None,
        agent: None,
        advisor: false,
        model: None,
        thinking_level: None,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 1,
        updated_ms: 1,
    };
    core.store_mut()
        .agent_chat
        .conversations
        .insert(conversation.id.clone(), conversation);
    let tab_id = "agent:abc".to_owned();
    let folder = DeckFolder {
        folder_key: FOLDER.to_owned(),
        live_session_ids: vec![tab_id.clone()],
    };
    deck(
        &mut core,
        DeckIntent::SelectTab {
            folder: folder.clone(),
            session_id: tab_id.clone(),
            compact: false,
        },
    );
    assert_eq!(navigated(&core), Some("/a/abc".to_owned()));
    assert!(find_leaf_of_tab(&stored_layout(&core).root, &tab_id).is_some());
    deck(
        &mut core,
        DeckIntent::CloseTab {
            folder: Some(folder),
            session_id: tab_id.clone(),
            active_session_id: Some(tab_id.clone()),
            labels: CloseLabels::default(),
        },
    );
    assert!(is_pending_close(core.store(), &tab_id));
    deck(
        &mut core,
        DeckIntent::UndoClose {
            session_id: tab_id.clone(),
        },
    );
    assert!(!is_pending_close(core.store(), &tab_id));
    assert_eq!(navigated(&core), Some("/a/abc".to_owned()));
    assert!(find_leaf_of_tab(&stored_layout(&core).root, &tab_id).is_some());
}
