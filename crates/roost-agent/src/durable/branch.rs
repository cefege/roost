//! Transfers canonical conversation documents into independent native forks.
//! Native history is materialized separately; the kernel's fork planner selects
//! document policies and commit boundaries without copying live tasks or queues.

use std::collections::BTreeMap;

use rpi_durable::{EntryId, ROOT_CONVERSATION_ID, StorageWrite};

use super::driver::DurableDriver;
use super::state::ProjectionState;
use super::storage::replay_batches;
use crate::error::AgentError;
use crate::runtime::AgentRuntime;

pub(crate) async fn fork_documents(
    runtime: &AgentRuntime,
    source_id: &str,
    destination_id: &str,
    sequence_mapping: &[(u64, u64)],
) -> Result<(), AgentError> {
    let batches = runtime.inner.store.durable_batches(source_id).await?;
    if batches.is_empty() {
        return Ok(());
    }
    let projection = ProjectionState::new();
    let mut sources = BTreeMap::new();
    for batch in &batches {
        for write in &batch.writes {
            if let StorageWrite::Entry { value } = write
                && value.conversation_id == ROOT_CONVERSATION_ID
                && let Some(sequence) = value
                    .data
                    .as_ref()
                    .and_then(|data| data.get("journalSequence"))
                    .and_then(serde_json::Value::as_u64)
            {
                sources.insert(sequence, value.id);
            }
        }
    }
    let (memory, _) = replay_batches(source_id, batches, &projection)?;
    sources.extend(
        projection
            .projected
            .lock()
            .map_err(|_| AgentError::Store("Durable fork provenance lock poisoned".into()))?
            .iter()
            .map(|(sequence, entry)| (*sequence, *entry)),
    );
    let Some((_, boundary)) = sources.iter().rev().find(|(sequence, _)| {
        sequence_mapping
            .binary_search_by_key(*sequence, |(source, _)| *source)
            .is_ok()
    }) else {
        return Ok(());
    };
    let boundary: EntryId = *boundary;
    runtime
        .claim_lease(destination_id)
        .await?
        .ok_or_else(|| AgentError::Fenced(destination_id.into()))?;
    let result = async {
        let driver = DurableDriver::open(runtime.clone(), destination_id.to_owned()).await?;
        let seeded = driver.seed_fork_documents(&memory, boundary).await;
        let closed = driver.close(false).await;
        seeded.and(closed)
    }
    .await;
    runtime.release_lease(destination_id).await;
    result
}
