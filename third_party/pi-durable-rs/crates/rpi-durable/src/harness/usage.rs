//! Per-conversation usage ledger (port of `packages/durable/src/harness/usage.ts`
//! @ `abe508e1b`).
//!
//! The `pi.usage` document records a conversation's own spend: assistant entries
//! and summarization attempts under `models`, keyed `provider/modelId`, and tool
//! results under `tools`, keyed by tool name.
//!
//! Upstream types the stored entries as `JsonRepresentation<Usage>` branded
//! values; the Rust port uses the concrete pi-ai [`Usage`] type directly.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use rpi_ai::types::Usage;
use serde::{Deserialize, Serialize};

use crate::chord::json::{JsonObject, JsonValue};
use crate::documents::{DocDefinition, DocScope, DocToken, define_doc};
use crate::harness::types::{read_state, write_state};
use crate::ids::ConversationId;
use crate::session::{DocOwner, SessionResult, Transaction};
use crate::types::{DocumentFork, DocumentHistory};

/// Ledger of one conversation's own spend.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageState {
    /// Assistant entries and summarization attempts, keyed `provider/modelId`.
    pub models: BTreeMap<String, Usage>,
    /// Tool results, keyed by tool name; their usage has no model identity.
    pub tools: BTreeMap<String, Usage>,
}

/// Bucket of a [`UsageState`] (upstream `keyof UsageState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageBucket {
    /// Assistant/summarization spend.
    Models,
    /// Tool spend.
    Tools,
}

fn initial() -> JsonObject {
    let state = UsageState::default();
    match serde_json::to_value(&state) {
        Ok(JsonValue::Object(object)) => object,
        // Invariant: a `UsageState` always serializes to a JSON object.
        _ => JsonObject::new(),
    }
}

/// Built-in usage document (`pi.usage`), conversation scope, history latest.
pub static USAGE_DOC: LazyLock<DocToken> = LazyLock::new(|| {
    define_doc(DocDefinition {
        kind: "pi.usage".to_string(),
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

/// Add `usage` to one bucket of the conversation's `pi.usage`, in the commit that
/// records the response.
pub async fn record_usage(
    tx: &mut Transaction,
    conversation_id: ConversationId,
    bucket: UsageBucket,
    key: &str,
    usage: &Usage,
) -> SessionResult<()> {
    let draft = tx
        .doc(&USAGE_DOC, DocOwner::Conversation(conversation_id), None)
        .await?;
    let mut state: UsageState = read_state(&draft)?;
    let bucket = match bucket {
        UsageBucket::Models => &mut state.models,
        UsageBucket::Tools => &mut state.tools,
    };
    match bucket.get_mut(key) {
        Some(total) => add_usage(total, usage),
        None => {
            bucket.insert(key.to_string(), usage.clone());
        }
    }
    write_state(&draft, &state)
}

/// Add every counter of `usage` to `total`; optional counters are added once
/// either side reports them.
pub fn add_usage(total: &mut Usage, usage: &Usage) {
    total.input += usage.input;
    total.output += usage.output;
    total.cache_read += usage.cache_read;
    total.cache_write += usage.cache_write;
    total.total_tokens += usage.total_tokens;
    total.cache_write1h = add_optional(total.cache_write1h, usage.cache_write1h);
    total.reasoning = add_optional(total.reasoning, usage.reasoning);
    total.cost.input += usage.cost.input;
    total.cost.output += usage.cost.output;
    total.cost.cache_read += usage.cost.cache_read;
    total.cost.cache_write += usage.cost.cache_write;
    total.cost.total += usage.cost.total;
}

fn add_optional(total: Option<u64>, addend: Option<u64>) -> Option<u64> {
    match (total, addend) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

/// Add every bucket of `state` into `sum`.
pub fn add_usage_state(sum: &mut UsageState, state: &UsageState) {
    for (key, usage) in &state.models {
        match sum.models.get_mut(key) {
            Some(total) => add_usage(total, usage),
            None => {
                sum.models.insert(key.clone(), usage.clone());
            }
        }
    }
    for (key, usage) in &state.tools {
        match sum.tools.get_mut(key) {
            Some(total) => add_usage(total, usage),
            None => {
                sum.tools.insert(key.clone(), usage.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;
    use rpi_ai::types::UsageCost;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input,
            output,
            total_tokens: input + output,
            cost: UsageCost {
                input: 1.0,
                output: 2.0,
                total: 3.0,
                ..UsageCost::default()
            },
            ..Usage::default()
        }
    }

    #[test]
    fn add_usage_sums_optional_counters_once_either_side_reports() {
        let mut total = usage(1, 2);
        total.reasoning = None;
        let mut addend = usage(3, 4);
        addend.reasoning = Some(5);
        addend.cache_write1h = Some(6);
        add_usage(&mut total, &addend);
        assert_eq!(total.input, 4);
        assert_eq!(total.output, 6);
        assert_eq!(total.total_tokens, 10);
        assert_eq!(total.reasoning, Some(5));
        assert_eq!(total.cache_write1h, Some(6));
        assert_eq!(total.cost.total, 6.0);
    }

    #[test]
    fn add_usage_state_merges_buckets() {
        let mut sum = UsageState::default();
        sum.models.insert("p/m".to_string(), usage(1, 1));
        let mut state = UsageState::default();
        state.models.insert("p/m".to_string(), usage(2, 2));
        state.tools.insert("read".to_string(), usage(3, 3));
        add_usage_state(&mut sum, &state);
        assert_eq!(sum.models["p/m"].input, 3);
        assert_eq!(sum.tools["read"].output, 3);
    }

    #[tokio::test]
    async fn record_usage_adds_into_the_document() {
        use crate::session::{ConversationOwnership, commit_fn, create_session};

        let session = create_session(Arc::new(MemoryStorage::new()), None);
        let context = crate::chord::context::BACKGROUND_CONTEXT.clone();
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
        let recording = session.commit(
            commit_fn(move |tx| {
                Box::pin(async move {
                    record_usage(tx, conversation, UsageBucket::Models, "p/m", &usage(1, 2))
                        .await?;
                    record_usage(tx, conversation, UsageBucket::Models, "p/m", &usage(3, 4))
                        .await?;
                    Ok(())
                })
            }),
            &context,
        );
        recording.await.unwrap();
        let value = session
            .snapshot(&USAGE_DOC, DocOwner::Conversation(conversation), &context)
            .await
            .unwrap()
            .unwrap();
        let state: UsageState = serde_json::from_value(JsonValue::Object(value)).unwrap();
        assert_eq!(state.models["p/m"].input, 4);
    }
}
