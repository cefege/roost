//! Conversation document fork copy planner (port of
//! `packages/durable/src/session/forks.ts` @ `abe508e1b`).
//!
//! A fork copies every conversation document of the source conversation whose
//! `fork` policy is `asOf` (selected at the fork entry's owning conversation
//! commit) or `current` (selected from the immediate parent's current state).
//! `initial` documents are definition-free and stay absent in the child.

use std::collections::HashSet;

use crate::documents::address_id;
use crate::ids::{ConversationId, DocumentId, EntryId};
use crate::session::{SessionError, SessionResult};
use crate::types::StorageExt;
use crate::types::{
    DocumentAddress, DocumentCopySource, DocumentCreate, DocumentFork, DocumentHistory,
    DocumentPoint, DocumentQuery, DocumentScope, Storage,
};

const SCAN_PAGE_SIZE: usize = 256;

/// One definition-free document copy to create with a forked conversation
/// (upstream `ForkDocumentCopy`).
#[derive(Debug, Clone, PartialEq)]
pub struct ForkDocumentCopy {
    /// New child incarnation to stamp.
    pub record: DocumentCreate,
    /// Exact persisted source.
    pub source: DocumentCopySource,
}

#[derive(Clone, Copy)]
enum ForkPolicy {
    AsOf,
    Current,
}

impl ForkPolicy {
    fn document_fork(self) -> DocumentFork {
        match self {
            ForkPolicy::AsOf => DocumentFork::AsOf,
            ForkPolicy::Current => DocumentFork::Current,
        }
    }
}

/// Select every persisted conversation document copied by one fork (upstream
/// `prepareForkDocumentCopies`).
pub async fn prepare_fork_document_copies(
    storage: &dyn Storage,
    mint_storage: &dyn Storage,
    parent_conversation_id: ConversationId,
    at: EntryId,
    child_conversation_id: ConversationId,
    context: &crate::chord::context::Context,
) -> SessionResult<Vec<ForkDocumentCopy>> {
    let entry = storage
        .entry_in_conversation(parent_conversation_id, at, context)
        .await?
        .ok_or_else(|| {
            SessionError::message(format!(
                "Entry {at} is not visible from conversation {parent_conversation_id}"
            ))
        })?;
    let mut copies = Vec::new();
    let mut copied_addresses = HashSet::new();
    // `asOf` copies follow the entry-owning ancestor at the entry's commit.
    collect_copies(
        storage,
        mint_storage,
        DocumentScope::Conversation {
            conversation_id: entry.entry.conversation_id,
        },
        DocumentPoint::At(entry.commit_seq),
        ForkPolicy::AsOf,
        child_conversation_id,
        &mut copies,
        &mut copied_addresses,
        context,
    )
    .await?;
    // `current` copies follow the immediate parent's live state.
    collect_copies(
        storage,
        mint_storage,
        DocumentScope::Conversation {
            conversation_id: parent_conversation_id,
        },
        DocumentPoint::CURRENT,
        ForkPolicy::Current,
        child_conversation_id,
        &mut copies,
        &mut copied_addresses,
        context,
    )
    .await?;
    Ok(copies)
}

#[allow(clippy::too_many_arguments)]
async fn collect_copies(
    storage: &dyn Storage,
    mint_storage: &dyn Storage,
    scope: DocumentScope,
    at: DocumentPoint,
    policy: ForkPolicy,
    child_conversation_id: ConversationId,
    copies: &mut Vec<ForkDocumentCopy>,
    copied_addresses: &mut HashSet<String>,
    context: &crate::chord::context::Context,
) -> SessionResult<()> {
    let mut cursor = None;
    loop {
        let page = storage
            .scan_documents(
                DocumentQuery {
                    scope: scope.clone(),
                    at,
                    kind: None,
                },
                SCAN_PAGE_SIZE,
                cursor,
                context,
            )
            .await?;
        for source in page.items {
            if !matches!(source.scope, DocumentScope::Conversation { .. })
                || source.fork != Some(policy.document_fork())
            {
                continue;
            }
            let id: DocumentId = mint_storage.mint_id_as().await?;
            let history = match source.history {
                Some(DocumentHistory::Rewindable) => DocumentHistory::Rewindable,
                _ => DocumentHistory::Latest,
            };
            let record = DocumentCreate {
                id,
                kind: source.kind.clone(),
                key: source.key.clone(),
                scope: DocumentScope::Conversation {
                    conversation_id: child_conversation_id,
                },
                history: Some(history),
                fork: source.fork,
            };
            let address = DocumentAddress {
                kind: record.kind.clone(),
                scope: record.scope.clone(),
                key: record.key.clone(),
            };
            let copy_address = address_id(&address);
            if !copied_addresses.insert(copy_address) {
                let member = match &record.key {
                    Some(key) => format!("{}/{}", record.kind, key),
                    None => record.kind.clone(),
                };
                return Err(SessionError::message(format!(
                    "Fork selects multiple source documents for {member}"
                )));
            }
            copies.push(ForkDocumentCopy {
                record,
                source: DocumentCopySource { id: source.id, at },
            });
        }
        cursor = page.next;
        if cursor.is_none() {
            break;
        }
    }
    Ok(())
}
