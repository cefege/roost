//! One model call with the harness's failure policy: rotate accounts on a
//! rate limit (at most 3 times), retry transient provider failures with
//! backoff, and stream events into the conversation's live assistant item.

use futures::StreamExt;
use roost_llm::{AssistantBlock, ChatRequest, LlmError, StopReason, StreamEvent, Usage};
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::runtime::AgentRuntime;

const MAX_ROTATIONS: usize = 3;

/// A finished model response.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ModelOutput {
    pub blocks: Vec<AssistantBlock>,
    pub usage: Usage,
    pub stop: StopReason,
}

impl ModelOutput {
    pub fn tool_calls(&self) -> Vec<(String, String, String)> {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                AssistantBlock::ToolCall {
                    call_id,
                    name,
                    args_json,
                    ..
                } => Some((call_id.clone(), name.clone(), args_json.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                AssistantBlock::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Where a call's stream events go.
pub(crate) trait StreamObserver: Send {
    /// A retry starts the response over.
    fn restart(&mut self);
    fn event(&mut self, event: &StreamEvent);
    /// Whether the response has reached the transcript. A committed response
    /// is never handed to another model.
    fn committed(&self) -> bool {
        false
    }
}

/// Discards events: judgments, compaction and advisor reviews.
pub(crate) struct Silent;

impl StreamObserver for Silent {
    fn restart(&mut self) {}
    fn event(&mut self, _event: &StreamEvent) {}
}

/// Tries the candidates in order and reports the index of the one whose outcome
/// is returned. A candidate that fails with an error another model may answer
/// falls over to the next only while nothing has reached the transcript: a
/// response that streamed visible output is never replayed.
pub(crate) async fn call_chain(
    runtime: &AgentRuntime,
    account_key: &str,
    attempts: &[ChatRequest],
    cancel: &CancellationToken,
    observer: &mut dyn StreamObserver,
) -> (usize, Result<ModelOutput, AgentError>) {
    let mut index = 0;
    loop {
        let Some(request) = attempts.get(index) else {
            let error = AgentError::FailedPrecondition("no model candidates".to_owned());
            return (index, Err(error));
        };
        match call_candidate(runtime, account_key, request, cancel, observer).await {
            Ok(output) => return (index, Ok(output)),
            Err(error)
                if index + 1 < attempts.len() && falls_over(&error) && !observer.committed() =>
            {
                let next = &attempts[index + 1];
                tracing::warn!(
                    account_key,
                    from_provider = %request.model.provider,
                    from_model = %request.model.id,
                    to_provider = %next.model.provider,
                    to_model = %next.model.id,
                    %error,
                    "candidate failed before output; falling over to the next candidate"
                );
                observer.restart();
                index += 1;
            }
            Err(error) => return (index, Err(error.into())),
        }
    }
}

/// Errors another model may answer where this one could not: a rate limit or
/// account refusal that rotation did not clear, or a transport failure that
/// retries did not.
pub(crate) fn falls_over(error: &LlmError) -> bool {
    matches!(error, LlmError::RateLimited { .. } | LlmError::Auth(_)) || error.is_transient()
}

/// One candidate's call: resolves its account, rotates past rate-limited or
/// refused accounts and retries transient failures.
async fn call_candidate(
    runtime: &AgentRuntime,
    account_key: &str,
    request: &ChatRequest,
    cancel: &CancellationToken,
    observer: &mut dyn StreamObserver,
) -> Result<ModelOutput, LlmError> {
    let llm = &runtime.inner.llm;
    let provider = request.model.provider.clone();
    let (mut credential_id, mut auth) = llm.resolve(&provider, account_key).await?;
    let mut rotations = 0;
    let mut transient_retries = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        let stream = llm.stream(credential_id, request.clone(), auth.clone(), cancel.clone());
        match collect(stream, cancel, observer).await {
            Ok(output) => return Ok(output),
            Err(error @ (LlmError::RateLimited { .. } | LlmError::Auth(_)))
                if rotations < MAX_ROTATIONS =>
            {
                rotations += 1;
                tracing::info!(
                    account_key,
                    provider,
                    rotations,
                    %error,
                    "rotating account after the provider refused it"
                );
                observer.restart();
                (credential_id, auth) = llm
                    .rotate(credential_id, &provider, account_key, &error)
                    .await?;
            }
            Err(error)
                if error.is_transient()
                    && transient_retries < runtime.inner.config.retry_backoff.len() =>
            {
                let wait = runtime.inner.config.retry_backoff[transient_retries];
                transient_retries += 1;
                tracing::warn!(account_key, %error, attempt = transient_retries, "transient provider failure; retrying");
                observer.restart();
                tokio::select! {
                    () = cancel.cancelled() => return Err(LlmError::Cancelled),
                    () = tokio::time::sleep(wait) => {}
                }
            }
            Err(error) => return Err(error),
        }
    }
}

async fn collect(
    mut stream: futures::stream::BoxStream<'static, Result<StreamEvent, LlmError>>,
    cancel: &CancellationToken,
    observer: &mut dyn StreamObserver,
) -> Result<ModelOutput, LlmError> {
    let mut blocks: Vec<(usize, AssistantBlock)> = Vec::new();
    let mut usage = Usage::default();
    let mut stop = StopReason::EndTurn;
    loop {
        let next = tokio::select! {
            () = cancel.cancelled() => return Err(LlmError::Cancelled),
            next = stream.next() => next,
        };
        let Some(event) = next else { break };
        let event = event?;
        observer.event(&event);
        match event {
            StreamEvent::BlockEnd { index, block } => blocks.push((index, block)),
            StreamEvent::Usage(update) => merge_usage(&mut usage, &update),
            StreamEvent::Stop(reason) => stop = reason,
            _ => {}
        }
    }
    if let StopReason::Error(message) = &stop {
        return Err(LlmError::Decode(message.clone()));
    }
    blocks.sort_by_key(|(index, _)| *index);
    Ok(ModelOutput {
        blocks: blocks.into_iter().map(|(_, block)| block).collect(),
        usage,
        stop,
    })
}

/// Providers report usage cumulatively and piecewise (input at the start,
/// output at the end); the largest value seen per field is the total.
fn merge_usage(total: &mut Usage, update: &Usage) {
    total.input = total.input.max(update.input);
    total.output = total.output.max(update.output);
    total.cache_read = total.cache_read.max(update.cache_read);
    total.cache_write = total.cache_write.max(update.cache_write);
}
