//! Port of `packages/ai/src/api/lazy.ts` @ pi 0.82.1 (2efa728).
//!
//! `lazy_stream`: returns a stream synchronously while async setup (auth
//! resolution) runs behind it. Setup failures terminate the stream with an
//! error event carrying a zero-usage error message.

use std::future::Future;

use futures::StreamExt;

use crate::auth::ModelsError;
use crate::types::{
    ApiKind, AssistantMessage, AssistantRole, ErrorReason, StopReason, StreamEvent, Usage,
};
use crate::utils::event_stream::AssistantMessageEventStream;

/// `createSetupErrorMessage` (lazy.ts): zero-usage error assistant message
/// stamped at the current wall clock.
pub fn create_setup_error_message(
    api: &ApiKind,
    provider: &str,
    model: &str,
    error_message: &str,
) -> AssistantMessage {
    create_setup_error_message_at(api, provider, model, error_message, now_ms())
}

/// `createSetupErrorMessage` with the caller's captured stream start
/// (`lazy.ts:39`: the error message carries the `startedAt` of the outer
/// stream, so it stays timed).
pub fn create_setup_error_message_at(
    api: &ApiKind,
    provider: &str,
    model: &str,
    error_message: &str,
    timestamp: i64,
) -> AssistantMessage {
    AssistantMessage {
        role: AssistantRole::Assistant,
        content: vec![],
        api: api.clone(),
        provider: provider.to_owned(),
        model: model.to_owned(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: StopReason::Error,
        error_message: Some(error_message.to_owned()),
        timestamp,
        deferred: None,
        end_turn: None,
        raw_stop_reason: None,
        duration_ms: None,
    }
}

/// `createSetupErrorMessage` for a concrete model.
pub fn create_setup_error_message_for_model(
    model: &crate::types::Model,
    error_message: &str,
) -> AssistantMessage {
    create_setup_error_message(&model.api, &model.provider, &model.id, error_message)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `forwardStream`: forwards all inner events, then ends with the inner
/// stream's result (if any).
async fn forward_stream(
    target: &AssistantMessageEventStream,
    mut source: AssistantMessageEventStream,
) {
    let result = source.result();
    while let Some(event) = source.next().await {
        target.push(event);
    }
    target.end(result.await);
}

/// `lazyStream`: see module docs.
pub fn lazy_stream<F>(model: &crate::types::Model, setup: F) -> AssistantMessageEventStream
where
    F: Future<Output = Result<AssistantMessageEventStream, ModelsError>> + Send + 'static,
{
    let started_at = now_ms();
    let outer = AssistantMessageEventStream::new();
    let task_outer = outer.clone();
    let err_model = model.clone();

    tokio::spawn(async move {
        match setup.await {
            Ok(inner) => forward_stream(&task_outer, inner).await,
            Err(error) => {
                let message = create_setup_error_message_at(
                    &err_model.api,
                    &err_model.provider,
                    &err_model.id,
                    &error.message,
                    started_at,
                );
                task_outer.push(StreamEvent::Error {
                    reason: ErrorReason::Error,
                    error: message.clone(),
                });
                task_outer.end(Some(message));
            }
        }
    });

    outer
}
