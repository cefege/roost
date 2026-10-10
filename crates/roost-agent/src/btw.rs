//! Ported from oh-my-pi packages/coding-agent/src/session/btw-turn.ts (MIT).
//! Answers a side question against current history without exposing it to the
//! conversation model's future transcript.

use roost_llm::{ChatRequest, Message};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::history;
use crate::model_call::{Silent, call_model};
use crate::prompts;
use crate::records::Entry;
use crate::runtime::{AgentRuntime, random_id};
use crate::turn;

pub(crate) async fn run(
    runtime: &AgentRuntime,
    id: &str,
    question: &str,
) -> Result<(), AgentError> {
    let question = question.trim();
    if question.is_empty() {
        return Err(AgentError::InvalidArgument("/btw needs a question".into()));
    }
    let item_id = random_id("btw-");
    runtime
        .append(
            id,
            Entry::Btw {
                item_id: item_id.clone(),
                question: question.to_owned(),
                answer: None,
                error: None,
            },
        )
        .await?;
    tracing::info!(conversation_id = id, item_id, "btw side question started");
    let result = answer(runtime, id, question).await;
    match result {
        Ok((text, provider, model_id, usage)) => {
            runtime
                .add_extra_usage(id, &provider, &model_id, &usage)
                .await;
            runtime
                .append(
                    id,
                    Entry::Btw {
                        item_id,
                        question: question.to_owned(),
                        answer: Some(text),
                        error: None,
                    },
                )
                .await?;
            tracing::info!(conversation_id = id, "btw side question answered");
        }
        Err(error) => {
            tracing::warn!(conversation_id = id, %error, "btw side question failed");
            runtime
                .append(
                    id,
                    Entry::Btw {
                        item_id,
                        question: question.to_owned(),
                        answer: None,
                        error: Some(error.to_string()),
                    },
                )
                .await?;
        }
    }
    Ok(())
}

async fn answer(
    runtime: &AgentRuntime,
    id: &str,
    question: &str,
) -> Result<(String, String, String, roost_llm::Usage), AgentError> {
    let record = runtime.record(id).await?;
    let model = turn::conversation_model(runtime, &record).await?;
    let entries = runtime.inner.store.entries(id).await?;
    let mut messages = history::build_messages(&entries, &model.info.provider);
    messages.push(Message::user_text(prompts::render(
        prompts::BTW,
        &[("question", question)],
    )));
    let request = ChatRequest {
        model: model.info.clone(),
        system: prompts::system_blocks(&record, &[], "", None),
        messages,
        tools: Vec::new(),
        thinking: record.thinking_level.clone(),
        session_id: format!("{}-btw", record.id),
        max_tokens: None,
    };
    let cancel = CancellationToken::new();
    let output = call_model(runtime, &request.session_id, &request, &cancel, &mut Silent).await?;
    Ok((
        output.text(),
        request.model.provider,
        request.model.id,
        output.usage,
    ))
}
