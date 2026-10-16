//! Ported from oh-my-pi packages/coding-agent/src/session/btw-turn.ts (MIT).
//! Answers a side question against current history without exposing it to the
//! conversation model's future transcript.

use roost_llm::{ChatRequest, Message};
use tokio_util::sync::CancellationToken;

use crate::compaction_text::render_for_summary;
use crate::error::AgentError;
use crate::history;
use crate::model_call::{Silent, call_chain};
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
    let models = turn::side_task_models(runtime, &record).await?;
    let entries = crate::journal::effective_entries(&runtime.inner.store.entries(id).await?);
    let (summary, window) = history::active_window(&entries);
    let context = format!(
        "Conversation context (quoted transcript):\n{}\n{}",
        summary.unwrap_or_default(),
        render_for_summary(window)
    );
    let system = prompts::system_blocks(&record, &[], "", None);
    let question_message = prompts::render(prompts::BTW, &[("question", question)]);
    let session_id = format!("{}-btw", record.id);
    let attempts: Vec<ChatRequest> = models
        .into_iter()
        .map(|model| {
            let messages = vec![
                Message::user_text(context.clone()),
                Message::user_text(question_message.clone()),
            ];
            let thinking = model
                .clamp_thinking_level(&record.thinking_level)
                .to_owned();
            ChatRequest {
                model,
                system: system.clone(),
                messages,
                tools: Vec::new(),
                thinking,
                session_id: session_id.clone(),
                max_tokens: None,
            }
        })
        .collect();
    let cancel = CancellationToken::new();
    let (used, result) = call_chain(runtime, &session_id, &attempts, &cancel, &mut Silent).await;
    let output = result?;
    let answered = &attempts[used].model;
    Ok((
        output.text(),
        answered.provider.clone(),
        answered.id.clone(),
        output.usage,
    ))
}
