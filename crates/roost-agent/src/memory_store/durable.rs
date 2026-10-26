//! In-memory native storage for durable kernel logs and chat projection.
//! The same owner fence, counter, CAS and atomic settlement contract applies
//! here as in the coordinator SQL store; tests reopen a kernel over these logs.

use super::{InMemoryAgentStore, MemoryState};
use crate::durable::{DurableBatch, DurableCommit, StorageWrite};
use crate::error::AgentError;
use crate::records::Entry;
use crate::traits::LeaseFence;

impl InMemoryAgentStore {
    pub(super) fn read_durable_batches(&self, id: &str) -> Result<Vec<DurableBatch>, AgentError> {
        self.with_state(|state| {
            if !state.conversations.contains_key(id) {
                return Err(AgentError::NotFound(id.into()));
            }
            Ok(state.durable_batches.get(id).cloned().unwrap_or_default())
        })?
    }

    pub(super) fn reserve_durable_id(
        &self,
        id: &str,
        fence: &LeaseFence,
    ) -> Result<u64, AgentError> {
        self.with_state(|state| {
            assert_owner(state, id, fence)?;
            let next = state.durable_ids.entry(id.into()).or_insert(2);
            if *next > 9_007_199_254_740_991 {
                return Err(AgentError::Store("Durable ID space exhausted".into()));
            }
            let minted = *next;
            *next += 1;
            Ok(minted)
        })?
    }

    pub(super) fn commit_durable(
        &self,
        id: &str,
        expected_sequence: u64,
        writes: &[StorageWrite],
        entries: &[Entry],
        fence: &LeaseFence,
    ) -> Result<DurableCommit, AgentError> {
        self.with_state(|state| {
            assert_owner(state, id, fence)?;
            let current = state
                .durable_batches
                .get(id)
                .and_then(|batches| batches.last())
                .map_or(0, |batch| batch.sequence);
            if current != expected_sequence {
                return Err(AgentError::Conflict(id.into()));
            }
            let sequence = current
                .checked_add(1)
                .ok_or_else(|| AgentError::Store("Durable commit sequence exhausted".into()))?;
            let mut entry_sequences = Vec::with_capacity(entries.len());
            let mut batch = DurableBatch {
                sequence,
                writes: writes.to_vec(),
                entry_sequences: Vec::with_capacity(entries.len()),
            };
            for entry in entries {
                entry_sequences.push(state.append(id, entry.clone()));
            }
            batch.entry_sequences.clone_from(&entry_sequences);
            state
                .durable_batches
                .entry(id.into())
                .or_default()
                .push(batch);
            Ok(DurableCommit {
                sequence,
                entry_sequences,
            })
        })?
    }
}

fn assert_owner(state: &MemoryState, id: &str, fence: &LeaseFence) -> Result<(), AgentError> {
    if !state.conversations.contains_key(id) {
        return Err(AgentError::NotFound(id.into()));
    }
    if !state.holds(id, fence)
        || state
            .leases
            .get(id)
            .is_none_or(|lease| lease.expires_ms <= crate::runtime::now_ms())
    {
        return Err(AgentError::Fenced(id.into()));
    }
    Ok(())
}
