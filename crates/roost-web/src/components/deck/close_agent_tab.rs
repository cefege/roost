//! Schedule agent conversation deletion through the deck's undoable close path.

use roost_client_core::ClientEvent;
#[cfg(target_arch = "wasm32")]
use roost_client_core::client::rpc::calls::agent_chat::DeleteAgentChat;
use roost_client_core::deck::{DeckFolder, DeckIntent, agent_tab_id};
use roost_client_core::store::layout::find_leaf_of_tab;
use roost_client_core::store::paths::folder_key_of;
use roost_client_core::store::selectors::deck_tab_ids_for_folder;

use crate::platform::BrowserWorkerPaths;
use crate::pump::Pump;
use crate::session_actions::close_labels_for_agent;

pub fn close_agent_tab(pump: Pump, conversation_id: String) {
    let (intent, tab_is_in_deck) = {
        let core = pump.core();
        let core = core.borrow();
        let store = core.store();
        let Some(conversation) = store.agent_chat.conversations.get(&conversation_id) else {
            return;
        };
        let worker_os = store
            .workers
            .get(&conversation.worker_fp)
            .map(|worker| worker.os.as_str());
        let folder_key = folder_key_of(
            &BrowserWorkerPaths,
            worker_os,
            &conversation.worker_fp,
            &conversation.cwd,
        );
        let tab_id = agent_tab_id(&conversation_id);
        let live_session_ids = deck_tab_ids_for_folder(store, &BrowserWorkerPaths, &folder_key);
        let folder = DeckFolder {
            folder_key: folder_key.clone(),
            live_session_ids,
        };
        let tab_is_in_deck = store
            .deck
            .records()
            .stored(&folder_key)
            .is_some_and(|layout| find_leaf_of_tab(&layout.root, &tab_id).is_some());
        (
            DeckIntent::CloseTab {
                folder: Some(folder),
                session_id: tab_id.clone(),
                active_session_id: Some(tab_id),
                labels: close_labels_for_agent(store, conversation),
            },
            tab_is_in_deck,
        )
    };
    if tab_is_in_deck {
        pump.dispatch(ClientEvent::Deck(intent));
        return;
    }

    #[cfg(target_arch = "wasm32")]
    {
        use roost_client_core::store::shell_intent::ShellIntent;

        wasm_bindgen_futures::spawn_local(async move {
            if let Err(error) = pump
                .rpc()
                .call(&DeleteAgentChat {
                    conversation_id: conversation_id.clone(),
                })
                .await
            {
                let message = format!("Agent conversation delete failed: {error}");
                tracing::warn!(target: "agent_chat", %message, "agent conversation delete refused");
                pump.dispatch(ClientEvent::Shell(ShellIntent::ActionFailed { message }));
            }
        });
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (pump, conversation_id);
}
