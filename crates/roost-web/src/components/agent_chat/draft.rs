//! Per-conversation composer drafts retained for this browser tab.
//! `surface` calls this hook; session storage survives a refresh without
//! sharing unfinished prompts with other tabs or the coordinator.

use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::KeyValueStore;

use crate::platform::SessionStorageKeyValueStore;

pub(super) fn use_conversation_draft(conversation_id: &str) -> Signal<String> {
    let storage = use_hook(|| Rc::new(SessionStorageKeyValueStore::new()));
    let key = format!("roost.agent.draft.{conversation_id}");
    let mut draft = use_signal(|| storage.get(&key).unwrap_or_default());
    let mut loaded_key = use_signal(|| key.clone());
    if *loaded_key.peek() != key {
        draft.set(storage.get(&key).unwrap_or_default());
        loaded_key.set(key.clone());
    }
    use_effect(use_reactive((&key,), move |(key,)| {
        let text = draft.read();
        if text.is_empty() {
            storage.remove(&key);
        } else {
            storage.set(&key, &text);
        }
    }));
    draft
}
