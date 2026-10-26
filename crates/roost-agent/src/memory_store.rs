//! An in-memory `AgentStore`: the harness's own tests and the coordinator's
//! fixtures run the real loop against it. It honours the same contract as the
//! database store, including cascading deletes of child conversations.

mod command_admission;
mod durable;

use std::collections::BTreeMap;
use std::sync::Mutex;

use futures::future::BoxFuture;
use roost_protocol::wire::agent_chat::AgentRunState;
use tokio_util::sync::CancellationToken;

use crate::admission::{LeaseRow, lease_expiry, next_lease_epoch};
use crate::error::AgentError;
use crate::records::{AgentSettings, ConversationRecord, Entry};
use crate::traits::{AdmitOutcome, AgentStore, CommandAdmission, LeaseFence};

#[derive(Debug, Default)]
pub struct InMemoryAgentStore {
    state: Mutex<MemoryState>,
}

#[derive(Debug, Default)]
struct MemoryState {
    conversations: BTreeMap<String, ConversationRecord>,
    entries: BTreeMap<String, Vec<(u64, Entry)>>,
    settings: AgentSettings,
    advisor_states: BTreeMap<String, String>,
    commands: BTreeMap<(String, String), Option<String>>,
    leases: BTreeMap<String, LeaseRow>,
    durable_batches: BTreeMap<String, Vec<crate::durable::DurableBatch>>,
    durable_ids: BTreeMap<String, u64>,
}

impl MemoryState {
    fn append(&mut self, id: &str, entry: Entry) -> u64 {
        let journal = self.entries.entry(id.to_owned()).or_default();
        let seq = journal.last().map_or(1, |(last, _)| last + 1);
        journal.push((seq, entry));
        seq
    }

    fn last_seq(&self, id: &str) -> u64 {
        self.entries
            .get(id)
            .and_then(|journal| journal.last())
            .map_or(0, |(seq, _)| *seq)
    }

    fn holds(&self, id: &str, fence: &LeaseFence) -> bool {
        self.leases
            .get(id)
            .is_some_and(|row| row.owner == fence.owner && row.epoch == fence.epoch)
    }
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
    fn durable_batches<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<crate::durable::DurableBatch>, AgentError>> {
        Box::pin(async move { self.read_durable_batches(id) })
    }

    fn mint_durable_id<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(async move { self.reserve_durable_id(id, fence) })
    }

    fn append_durable_batch<'a>(
        &'a self,
        id: &'a str,
        expected_sequence: u64,
        writes: &'a [crate::durable::StorageWrite],
        entries: &'a [Entry],
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<crate::durable::DurableCommit, AgentError>> {
        Box::pin(async move { self.commit_durable(id, expected_sequence, writes, entries, fence) })
    }

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
        expected_revision: u64,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                let stored = state.conversations.get(&record.id).map(|row| row.revision);
                let matches = match stored {
                    None => expected_revision == 0,
                    Some(revision) => revision == expected_revision && expected_revision != 0,
                };
                if !matches {
                    return Err(AgentError::Conflict(record.id.clone()));
                }
                let mut saved = record.clone();
                saved.revision = expected_revision + 1;
                state.conversations.insert(record.id.clone(), saved);
                Ok(expected_revision + 1)
            })?
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
                            .filter(|record| record.is_child_of(&parent))
                            .map(|record| record.id.clone()),
                    );
                    cursor += 1;
                }
                for conversation_id in doomed {
                    state.conversations.remove(&conversation_id);
                    state.entries.remove(&conversation_id);
                    state.advisor_states.remove(&conversation_id);
                    state.durable_batches.remove(&conversation_id);
                    state.durable_ids.remove(&conversation_id);
                    state.leases.remove(&conversation_id);
                    state
                        .commands
                        .retain(|(conversation, _), _| *conversation != conversation_id);
                }
            })
        })
    }

    fn append_entry<'a>(
        &'a self,
        id: &'a str,
        entry: &'a Entry,
        cancellation: &'a [&'a CancellationToken],
        fence: Option<&'a LeaseFence>,
    ) -> BoxFuture<'a, Result<u64, AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                if cancellation.iter().any(|token| token.is_cancelled()) {
                    return Err(AgentError::Cancelled);
                }
                if fence.is_some_and(|fence| !state.holds(id, fence)) {
                    return Err(AgentError::Fenced(id.to_owned()));
                }
                Ok(state.append(id, entry.clone()))
            })?
        })
    }

    fn admit_command<'a>(
        &'a self,
        id: &'a str,
        admission: CommandAdmission,
    ) -> BoxFuture<'a, Result<AdmitOutcome, AgentError>> {
        Box::pin(async move { self.with_state(|state| state.admit_command(id, admission))? })
    }

    fn complete_command<'a>(
        &'a self,
        id: &'a str,
        request_id: &'a str,
        result_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                state.commands.insert(
                    (id.to_owned(), request_id.to_owned()),
                    Some(result_json.to_owned()),
                );
            })
        })
    }

    fn acquire_lease<'a>(
        &'a self,
        id: &'a str,
        owner: &'a str,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<Option<u64>, AgentError>> {
        Box::pin(async move {
            let now = crate::runtime::now_ms();
            self.with_state(|state| {
                if !state.conversations.contains_key(id) {
                    return Err(AgentError::NotFound(id.to_owned()));
                }
                let Some(epoch) = next_lease_epoch(state.leases.get(id), owner, now) else {
                    return Ok(None);
                };
                state.leases.insert(
                    id.to_owned(),
                    LeaseRow {
                        owner: owner.to_owned(),
                        epoch,
                        expires_ms: lease_expiry(now, ttl_ms),
                    },
                );
                Ok(Some(epoch))
            })?
        })
    }

    fn renew_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
        ttl_ms: u64,
    ) -> BoxFuture<'a, Result<bool, AgentError>> {
        Box::pin(async move {
            let now = crate::runtime::now_ms();
            self.with_state(|state| {
                if !state.holds(id, fence) {
                    return false;
                }
                if let Some(row) = state.leases.get_mut(id) {
                    row.expires_ms = lease_expiry(now, ttl_ms);
                }
                true
            })
        })
    }

    fn release_lease<'a>(
        &'a self,
        id: &'a str,
        fence: &'a LeaseFence,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                if state.holds(id, fence)
                    && let Some(row) = state.leases.get_mut(id)
                {
                    row.expires_ms = 0;
                }
            })
        })
    }

    fn orphaned_runs(&self, now_ms: i64) -> BoxFuture<'_, Result<Vec<String>, AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                state
                    .conversations
                    .values()
                    .filter(|record| {
                        let lease = state.leases.get(&record.id);
                        let crashed =
                            lease.is_some_and(|row| row.expires_ms > 0 && row.expires_ms <= now_ms);
                        let unowned = lease.is_none_or(|row| row.expires_ms <= now_ms);
                        crashed || (record.run_state == AgentRunState::Running && unowned)
                    })
                    .map(|record| record.id.clone())
                    .collect()
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

    fn advisor_state<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, AgentError>> {
        Box::pin(async move { self.with_state(|state| state.advisor_states.get(id).cloned()) })
    }

    fn save_advisor_state<'a>(
        &'a self,
        id: &'a str,
        state_json: &'a str,
    ) -> BoxFuture<'a, Result<(), AgentError>> {
        Box::pin(async move {
            self.with_state(|state| {
                state
                    .advisor_states
                    .insert(id.to_owned(), state_json.to_owned());
            })
        })
    }
}
