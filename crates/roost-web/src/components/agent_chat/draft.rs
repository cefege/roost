//! Per-conversation composer drafts, synced across this operator's laptops.
//! `surface` calls this hook; the draft lives under an allowlisted client-state
//! key, so an unsent prompt follows the operator to another laptop.

use dioxus::prelude::*;

use crate::pump::{Pump, use_pump};

pub(super) fn use_conversation_draft(conversation_id: &str) -> Signal<String> {
    let pump = use_pump();
    let key = format!("roost.agent.draft.{conversation_id}");
    let mut draft = use_signal(|| stored_draft(&pump, &key));
    let mut loaded_key = use_signal(|| key.clone());
    if *loaded_key.peek() != key {
        draft.set(stored_draft(&pump, &key));
        loaded_key.set(key.clone());
    }
    let core = pump.core();
    use_effect(use_reactive((&key,), move |(key,)| {
        let text = draft.read();
        let core = core.borrow();
        if text.is_empty() {
            core.storage().remove(&key);
        } else {
            core.storage().set(&key, &text);
        }
    }));
    draft
}

fn stored_draft(pump: &Pump, key: &str) -> String {
    let core = pump.core();
    let core = core.borrow();
    core.storage().get(key).unwrap_or_default()
}
