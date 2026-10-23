//! Browser-tab persistence for one unresolved logical message submission.
//! The composer records identity before dispatch; retries and reloads reuse it
//! even when the live run state changes. No prompt or identifier is logged.

use dioxus::prelude::*;
use roost_client_core::KeyValueStore;
use serde::{Deserialize, Serialize};

use crate::platform::SessionStorageKeyValueStore;

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PendingSubmission {
    pub text: String,
    pub when_busy: String,
    pub request_id: String,
}

pub(super) fn restore(conversation_id: &str) -> Option<PendingSubmission> {
    let key = format!("roost.agent.pending.{conversation_id}");
    SessionStorageKeyValueStore::new()
        .get(&key)
        .and_then(|stored| serde_json::from_str(&stored).ok())
}

pub(super) fn prepare(
    conversation_id: &str,
    text: &str,
    selection: &str,
    previous: Option<&PendingSubmission>,
) -> PendingSubmission {
    let pending = match previous {
        Some(previous) if previous.text == text && previous.when_busy == selection => {
            previous.clone()
        }
        _ => PendingSubmission {
            text: text.to_owned(),
            when_busy: selection.to_owned(),
            request_id: super::request_ids::new_request_id(),
        },
    };
    if let Ok(encoded) = serde_json::to_string(&pending) {
        SessionStorageKeyValueStore::new()
            .set(&format!("roost.agent.pending.{conversation_id}"), &encoded);
    }
    pending
}

pub(super) fn settle(conversation_id: &str, clear_draft: bool) {
    let storage = SessionStorageKeyValueStore::new();
    if clear_draft {
        storage.remove(&format!("roost.agent.draft.{conversation_id}"));
    }
    storage.remove(&format!("roost.agent.pending.{conversation_id}"));
}

pub(super) fn completion_callback(
    conversation_id: String,
    mut draft: Signal<String>,
    mut pending: Signal<bool>,
    mut unresolved: Signal<Option<PendingSubmission>>,
    report: EventHandler<Result<(), String>>,
) -> EventHandler<Result<(), String>> {
    EventHandler::new(move |result: Result<(), String>| {
        pending.set(false);
        if result.is_ok() {
            let clear_draft = unresolved
                .peek()
                .as_ref()
                .is_some_and(|submission| draft.peek().trim() == submission.text.as_str());
            settle(&conversation_id, clear_draft);
            if clear_draft {
                draft.set(String::new());
            }
            unresolved.set(None);
        }
        report.call(result);
    })
}
