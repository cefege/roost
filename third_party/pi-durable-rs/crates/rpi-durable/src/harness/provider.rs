//! Stable provider-facing conversation identity
//! (port of `packages/durable/src/harness/provider.ts` @ `abe508e1b`).
//!
//! Every conversation carries a `pi.provider` document with a fresh identity;
//! forks start with a new one instead of copying their parent.
//!
//! Upstream `ensureProviderSessionId` takes the scheduler's `TaskRuntime`; the
//! Rust port takes the session kernel directly and commits the legacy migration
//! with no task scope, which is equivalent for this read/migrate pair.

use std::sync::{Arc, LazyLock};

use rpi_ai::utils::uuid::uuidv7;
use serde::{Deserialize, Serialize};

use crate::chord::context::Context;
use crate::chord::json::{JsonObject, JsonValue};
use crate::documents::{DocDefinition, DocScope, DocToken, define_doc};
use crate::ids::ConversationId;
use crate::session::{
    DocOwner, SessionError, SessionFuture, SessionImpl, SessionResult, commit_fn,
};
use crate::types::{DocumentFork, DocumentHistory};

/// Stable provider-facing identity of one conversation (`ProviderState`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderState {
    /// Provider session ID.
    pub session_id: String,
}

fn initial() -> JsonObject {
    let state = ProviderState {
        // Invariant: `uuidv7` only fails on sequence exhaustion or an
        // out-of-range timestamp; a fresh identity is always time-now.
        session_id: uuidv7(None).unwrap_or_default(),
    };
    match serde_json::to_value(&state) {
        Ok(JsonValue::Object(object)) => object,
        // Invariant: a `ProviderState` always serializes to a JSON object.
        _ => JsonObject::new(),
    }
}

/// Built-in provider state (`pi.provider`), conversation scope, fresh fork.
pub static PROVIDER_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.provider".to_string(),
        version: 1,
        scope: DocScope::Conversation,
        history: Some(DocumentHistory::Latest),
        fork: Some(DocumentFork::Initial),
        family: false,
        initial: Arc::new(|_seed| initial()),
        migrate: None,
        checkpoint_when: Some(Arc::new(|_value, _ops, _revision| true)),
    })
});

/// Return the persisted identity without writing in the normal path.
///
/// A legacy conversation without `pi.provider` gets one migration commit whose
/// `tx.doc()` runs `initial()` before the provider request starts.
pub async fn ensure_provider_session_id(
    session: &Arc<SessionImpl>,
    conversation_id: ConversationId,
    context: &Context,
) -> SessionResult<String> {
    let existing = session
        .snapshot(
            &PROVIDER_DOC,
            DocOwner::Conversation(conversation_id),
            context,
        )
        .await?;
    if let Some(value) = existing
        && let Some(session_id) = value.get("sessionId").and_then(JsonValue::as_str)
    {
        return Ok(session_id.to_string());
    }
    let change: crate::session::CommitFn<Option<String>> =
        commit_fn(move |tx| -> SessionFuture<'_, Option<String>> {
            Box::pin(async move {
                let draft = tx
                    .doc(&PROVIDER_DOC, DocOwner::Conversation(conversation_id), None)
                    .await?;
                let value = draft.read()?;
                Ok(value
                    .get("sessionId")
                    .and_then(JsonValue::as_str)
                    .map(str::to_string))
            })
        });
    let created = session.commit_with(change, context, None).await?;
    created.ok_or_else(|| {
        SessionError::message(format!(
            "Conversation {conversation_id} has no provider session ID"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chord::context::BACKGROUND_CONTEXT;
    use crate::session::{ConversationOwnership, create_session};
    use crate::storage::MemoryStorage;

    #[tokio::test]
    async fn ensure_reads_without_writing_then_migrates_legacy() {
        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let context = BACKGROUND_CONTEXT.clone();
        let conversation = session
            .commit(
                commit_fn(|tx| {
                    Box::pin(async move {
                        let record = tx
                            .create_conversation(ConversationOwnership::Ownerless)
                            .await?;
                        Ok(record.id)
                    })
                }),
                &context,
            )
            .await
            .unwrap();
        let first = ensure_provider_session_id(&session, conversation, &context)
            .await
            .unwrap();
        assert!(!first.is_empty());
        let second = ensure_provider_session_id(&session, conversation, &context)
            .await
            .unwrap();
        assert_eq!(first, second);
    }
}
