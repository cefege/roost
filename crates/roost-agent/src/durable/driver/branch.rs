//! Seeds a fresh native namespace with the kernel-selected fork document state.
//! Called before publishing the fork; task documents and live queues stay fresh.
//! Cross-namespace copies become base checkpoints, not dangling source references.

use rpi_durable::session::forks::prepare_fork_document_copies;
use rpi_durable::types::{DocumentAddress, DocumentContent, DocumentPoint};
use rpi_durable::{
    BACKGROUND_CONTEXT, EntryId, MemoryStorage, ROOT_CONVERSATION_ID, Storage, StorageWrite,
};

use super::DurableDriver;
use crate::error::AgentError;

impl DurableDriver {
    pub(crate) async fn seed_fork_documents(
        &self,
        source: &MemoryStorage,
        boundary: EntryId,
    ) -> Result<(), AgentError> {
        let copies = prepare_fork_document_copies(
            source,
            self.storage.as_ref(),
            ROOT_CONVERSATION_ID,
            boundary,
            ROOT_CONVERSATION_ID,
            &BACKGROUND_CONTEXT,
        )
        .await
        .map_err(super::session_error)?;
        let mut writes = Vec::with_capacity(copies.len() * 2);
        for copy in copies {
            let stored = source
                .document(copy.source.id, copy.source.at, &BACKGROUND_CONTEXT)
                .await
                .map_err(storage_error)?
                .ok_or_else(|| {
                    AgentError::Store("Durable fork document source is absent".into())
                })?;
            let address = DocumentAddress {
                kind: copy.record.kind.clone(),
                scope: copy.record.scope.clone(),
                key: copy.record.key.clone(),
            };
            if let Some(previous) = self
                .storage
                .find_document(address, DocumentPoint::CURRENT, &BACKGROUND_CONTEXT)
                .await
                .map_err(storage_error)?
            {
                writes.push(StorageWrite::DocumentRetire { id: previous.id });
            }
            writes.push(StorageWrite::DocumentCreate {
                record: copy.record,
                content: DocumentContent::Base {
                    version: stored.version,
                    value: stored.value,
                },
            });
        }
        if !writes.is_empty() {
            self.storage
                .commit(&writes, &BACKGROUND_CONTEXT)
                .await
                .map_err(storage_error)?;
        }
        Ok(())
    }
}

fn storage_error(error: impl std::fmt::Display) -> AgentError {
    AgentError::Store(error.to_string())
}
