//! Fenced SQL-log storage for the reusable Durable kernel.
//! The native AgentStore persists batches and chat entries atomically; upstream's
//! prepared MemoryStorage supplies the same indexed query semantics as JSONL.

mod reads;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rpi_durable::{
    Context, MemoryStorage, Seq, Storage, StorageError, StorageResult, StorageWrite,
};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use super::state::ProjectionState;
use crate::error::AgentError;
use crate::runtime::AgentRuntime;
use crate::traits::LeaseFence;

/// One atomically settled Durable batch, ordered within its native owner scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableBatch {
    pub sequence: u64,
    pub entry_sequences: Vec<u64>,
    pub writes: Vec<StorageWrite>,
}

/// Settlement receipt linking a kernel commit to its native transcript projection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableCommit {
    pub sequence: u64,
    pub entry_sequences: Vec<u64>,
}

pub(super) struct JournalStorage {
    runtime: AgentRuntime,
    conversation_id: String,
    fence: LeaseFence,
    memory: MemoryStorage,
    sequence: Mutex<u64>,
    closed: AtomicBool,
    projection: Arc<ProjectionState>,
}

impl std::fmt::Debug for JournalStorage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("JournalStorage")
            .field("conversation_id", &self.conversation_id)
            .field("fence", &self.fence)
            .field("closed", &self.closed.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}

impl JournalStorage {
    pub async fn open(
        runtime: AgentRuntime,
        conversation_id: String,
        fence: LeaseFence,
        projection: Arc<ProjectionState>,
    ) -> Result<Arc<Self>, AgentError> {
        let batches = runtime
            .inner
            .store
            .durable_batches(&conversation_id)
            .await?;
        let (memory, sequence) = replay_batches(&conversation_id, batches, &projection)?;
        tracing::info!(
            conversation_id,
            sequence,
            epoch = fence.epoch,
            "durable storage opened"
        );
        Ok(Arc::new(Self {
            runtime,
            conversation_id,
            fence,
            memory,
            sequence: Mutex::new(sequence),
            closed: AtomicBool::new(false),
            projection,
        }))
    }

    fn check(&self, context: &Context) -> StorageResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(StorageError::Closed("Roost Durable storage"));
        }
        if context
            .abort_signal()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(StorageError::Rejected(
                "Operation cancelled before storage admission".into(),
            ));
        }
        Ok(())
    }

    pub(super) async fn commit_batch(
        &self,
        writes: &[StorageWrite],
        context: &Context,
    ) -> StorageResult<Seq> {
        let mut sequence = self.sequence.lock().await;
        self.check(context)?;
        let next = sequence
            .checked_add(1)
            .ok_or_else(|| StorageError::message("Durable commit sequence exhausted"))?;
        let mut prepared = self
            .memory
            .prepare_commit(writes.to_vec(), Some(rpi_durable::seq_from_number(next)))?;
        let entries = super::projection::entries(prepared.writes(), &self.projection)
            .map_err(storage_error)?;
        let committed = self
            .runtime
            .inner
            .store
            .append_durable_batch(
                &self.conversation_id,
                *sequence,
                prepared.writes(),
                &entries,
                &self.fence,
            )
            .await
            .map_err(|error| {
                self.closed.store(true, Ordering::Release);
                if matches!(error, AgentError::Fenced(_)) {
                    self.runtime.lease_lost(&self.conversation_id);
                }
                storage_error(error)
            })?;
        if committed.sequence != next || committed.entry_sequences.len() != entries.len() {
            self.closed.store(true, Ordering::Release);
            return Err(StorageError::message(
                "Invalid native Durable settlement receipt",
            ));
        }
        let adopted = self.memory.apply_prepared(&mut prepared).inspect_err(|_| {
            self.closed.store(true, Ordering::Release);
        })?;
        *sequence = committed.sequence;
        record_origins(
            &self.projection,
            prepared.writes(),
            &committed.entry_sequences,
        )
        .map_err(storage_error)?;
        let tool_call_boundaries = entries
            .iter()
            .zip(&committed.entry_sequences)
            .filter_map(|(entry, sequence)| match entry {
                crate::records::Entry::Assistant {
                    blocks,
                    metadata: Some(metadata),
                    ..
                } if metadata.completed
                    && blocks.iter().any(|block| {
                        matches!(block, roost_llm::AssistantBlock::ToolCall { .. })
                    }) =>
                {
                    Some((*sequence, metadata.input_through_seq))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        for (assistant_seq, input_through_seq) in tool_call_boundaries {
            self.runtime.advisor_turn_ended(
                &self.conversation_id,
                true,
                Some(crate::advisor::AssistantResponseBoundary {
                    assistant_seq,
                    input_through_seq,
                    terminal: false,
                }),
            );
        }
        for (entry, sequence) in entries.iter().zip(committed.entry_sequences) {
            if let Err(error) = self
                .runtime
                .emit_entry(&self.conversation_id, sequence, entry)
                .await
            {
                tracing::warn!(conversation_id = self.conversation_id, sequence, %error,
                    "durable chat publication deferred to snapshot recovery");
            }
        }
        if entries.iter().any(|entry| {
            matches!(
                entry,
                crate::records::Entry::Assistant { .. } | crate::records::Entry::ExtraUsage { .. }
            )
        }) {
            match self.runtime.usage_totals(&self.conversation_id).await {
                Ok(usage) => {
                    self.runtime
                        .emit(
                            &self.conversation_id,
                            vec![roost_protocol::wire::agent_chat::ChatEvent::Usage { usage }],
                        )
                        .await
                }
                Err(error) => tracing::warn!(conversation_id = self.conversation_id, %error,
                    "durable usage publication deferred to snapshot recovery"),
            }
        }
        Ok(adopted)
    }

    pub(super) async fn reserve_id(&self) -> StorageResult<u64> {
        self.check(&rpi_durable::BACKGROUND_CONTEXT)?;
        self.runtime
            .inner
            .store
            .mint_durable_id(&self.conversation_id, &self.fence)
            .await
            .map_err(|error| {
                self.closed.store(true, Ordering::Release);
                if matches!(error, AgentError::Fenced(_)) {
                    self.runtime.lease_lost(&self.conversation_id);
                }
                storage_error(error)
            })
    }

    pub(super) async fn close_storage(&self, context: &Context) -> StorageResult<()> {
        let _guard = self.sequence.lock().await;
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.memory.close(context).await?;
            tracing::info!(
                conversation_id = self.conversation_id,
                "durable storage closed"
            );
        }
        Ok(())
    }
}

pub(super) fn replay_batches(
    conversation_id: &str,
    batches: Vec<DurableBatch>,
    projection: &ProjectionState,
) -> Result<(MemoryStorage, u64), AgentError> {
    let memory = MemoryStorage::new();
    let mut sequence = 0_u64;
    for batch in batches {
        if batch.sequence != sequence + 1 {
            return Err(AgentError::Store(format!(
                "Durable commit sequence is discontinuous for {conversation_id}"
            )));
        }
        record_origins(projection, &batch.writes, &batch.entry_sequences)?;
        let mut prepared = memory
            .prepare_commit(
                batch.writes,
                Some(rpi_durable::seq_from_number(batch.sequence)),
            )
            .map_err(agent_error)?;
        memory.apply_prepared(&mut prepared).map_err(agent_error)?;
        sequence = batch.sequence;
    }
    Ok((memory, sequence))
}

fn storage_error(error: impl std::fmt::Display) -> StorageError {
    StorageError::message(error.to_string())
}

fn agent_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Store(error.to_string())
}

fn record_origins(
    projection: &ProjectionState,
    writes: &[StorageWrite],
    sequences: &[u64],
) -> Result<(), AgentError> {
    let ids = super::projection::projected_ids(writes);
    // Side-task rows follow entry projections and never contribute to model history.
    if ids.len() > sequences.len() {
        return Err(AgentError::Store(
            "Durable projection provenance is inconsistent".into(),
        ));
    }
    let mut projected = projection
        .projected
        .lock()
        .map_err(|_| AgentError::Store("Durable projection lock poisoned".into()))?;
    let mut origins = projection
        .origins
        .lock()
        .map_err(|_| AgentError::Store("Durable origin lock poisoned".into()))?;
    for (id, sequence) in ids.into_iter().zip(sequences) {
        projected.insert(*sequence, id);
        origins.insert(id, *sequence);
    }
    Ok(())
}
