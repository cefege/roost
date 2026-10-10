//! An in-memory `AgentStore`: the harness's own tests and the coordinator's
//! fixtures run the real loop against it. It honours the same contract as the
//! database store, including cascading deletes of child conversations.

use std::collections::BTreeMap;
use std::sync::Mutex;

use futures::future::BoxFuture;

use crate::error::AgentError;
use crate::records::{AgentSettings, ConversationRecord, Entry};
use crate::traits::AgentStore;

#[derive(Debug, Default)]
pub struct InMemoryAgentStore {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    conversations: BTreeMap<String, ConversationRecord>,
    entries: BTreeMap<String, Vec<(u64, Entry)>>,
    settings: AgentSettings,
}

impl InMemoryAgentStore {
    fn with_state<T>(&self, read: impl FnOnce(&mut MemoryState) -> T) -> Result<T, AgentError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| AgentError::Store("in-memory store lock poisoned".into()))?;
        Ok(read(&mut state))
    }
}

impl AgentStore for InMemoryAgentStore {
    fn conversations(&self) -> BoxFuture<'_, Result<Vec<ConversationRecord>, AgentError>> {
        Box::pin(
            async move { self.with_state(|state| state.conversations.values().cloned().collect()) },
        )
    }

    fn conversation<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationRecord>, AgentError>> {
        Box::pin(async move { self.with_state(|state| state.conversations.get(id).cloned()) })
    }

    fn save_conversation<'a>(
        &'a self,
        record: &'a ConversationRecord,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                state
                    .conversations
                    .insert(record.id.clone(), record.clone());
            })
        })
    }

    fn delete_conversation<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                let mut doomed = vec![id.to_owned()];
                let mut cursor = 0;
                while let Some(parent) = doomed.get(cursor).cloned() {
                    doomed.extend(
                        state
                            .conversations
                            .values()
                            .filter(|record| record.parent_id.as_deref() == Some(parent.as_str()))
                            .map(|record| record.id.clone()),
                    );
                    cursor += 1;
                }
                for conversation_id in doomed {
                    state.conversations.remove(&conversation_id);
                    state.entries.remove(&conversation_id);
                }
            })
        })
    }

    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                let journal = state.entries.entry(id.to_owned()).or_default();
                let seq = journal.last().map_or(1, |(last, _)| last + 1);
                journal.push((seq, entry.clone()));
                seq
            })
        })
    }

    fn entries<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Vec<(u64, Entry)>, AgentError>> {
        Box::pin(async move {
            self.with_state(|state| state.entries.get(id).cloned().unwrap_or_default())
        })
    }

    fn settings(&self) -> BoxFuture<'_, Result<AgentSettings, AgentError>> {
        Box::pin(async move { self.with_state(|state| state.settings.clone()) })
    }

    fn save_settings<'a>(
        &'a self,
        settings: &'a AgentSettings,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                state.settings = settings.clone();
            })
        })
    }
}
