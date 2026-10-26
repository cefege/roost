//! Pi's model-provider boundary backed by Roost's account-aware LLM path.
//!
//! Every provider call enters `call_chain`; the adapter never writes assistant
//! records, leaving durable turns as the only persistence owner.

use roost_llm::Message as LlmMessage;
use rpi_ai::models::ProviderStreams;
use rpi_ai::types::{
    DoneReason, ErrorReason, Model, RequestOriginKind, StreamEvent, StreamOptions,
    TranscriptContext,
};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::live_item::LiveItem;
use crate::model_call::call_chain;
use crate::records::Entry;
use crate::runtime::AgentRuntime;
use crate::runtime::RunEnd;

use super::state::ProjectionState;

#[path = "model_auth.rs"]
mod model_auth;
#[path = "model_classifier.rs"]
mod model_classifier;

#[path = "model_catalog.rs"]
mod model_catalog;
#[path = "model_request.rs"]
mod model_request;
use model_request::{pi_thinking_level, requests_from_context};
#[path = "model_observer.rs"]
mod model_observer;
use model_observer::{FanoutObserver, PiObserver};
#[path = "model_response.rs"]
mod model_response;
use model_response::{assistant_message, error_message};

#[path = "model_registration.rs"]
mod model_registration;
pub(crate) use model_registration::models;

struct RoostProvider {
    runtime: AgentRuntime,
    conversation_id: String,
    projection: Arc<ProjectionState>,
}

impl ProviderStreams for RoostProvider {
    fn stream(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        let stream = AssistantMessageEventStream::new();
        let output = stream.clone();
        let runtime = self.runtime.clone();
        let conversation_id = self.conversation_id.clone();
        let projection = Arc::clone(&self.projection);
        let model = model.clone();
        let context = context.clone();
        let request_options = options.clone().unwrap_or_default();
        let cancel = request_options.request.signal.clone().unwrap_or_default();
        tokio::spawn(async move {
            execute_provider_call(
                runtime,
                conversation_id,
                projection,
                model,
                context,
                request_options,
                cancel,
                output,
            )
            .await;
        });
        stream
    }
    fn stream_simple(
        &self,
        model: &Model,
        context: &TranscriptContext,
        options: Option<rpi_ai::types::SimpleStreamOptions>,
    ) -> Result<AssistantMessageEventStream, String> {
        let options = options.map(|mut options| {
            options.stream.reasoning = options.reasoning.map(|level| level.to_model_level());
            options.stream
        });
        Ok(self.stream(model, context, options))
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_provider_call(
    runtime: AgentRuntime,
    conversation_id: String,
    projection: Arc<ProjectionState>,
    model: Model,
    context: TranscriptContext,
    options: StreamOptions,
    cancel: CancellationToken,
    stream: AssistantMessageEventStream,
) {
    let cancellation =
        super::cancellation::InvocationCancellation::new(&runtime, &conversation_id, cancel);
    let cancel = cancellation.token();
    let is_compaction = options
        .origin
        .as_ref()
        .is_some_and(|origin| origin.kind == RequestOriginKind::Compaction);
    if !is_compaction {
        *projection
            .end
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = None;
    }
    let attempts =
        match requests_from_context(&runtime, &conversation_id, &model, &context, &options).await {
            Ok(attempts) => attempts,
            Err(error) => {
                if !is_compaction {
                    *projection
                        .end
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner()) =
                        Some(RunEnd::Failed(error.to_string()));
                }
                stream.push(StreamEvent::Error {
                    reason: ErrorReason::Error,
                    error: error_message(&model, error.to_string()),
                });
                return;
            }
        };
    let compaction_task_id = match options
        .origin
        .as_ref()
        .filter(|origin| origin.kind == RequestOriginKind::Compaction)
    {
        Some(origin) => match origin.id.parse::<u64>() {
            Ok(task_id) => Some(task_id),
            Err(error) => {
                let message = format!("invalid compaction request origin: {error}");
                stream.push(StreamEvent::Error {
                    reason: ErrorReason::Error,
                    error: error_message(&model, message),
                });
                return;
            }
        },
        None => None,
    };
    let mut partial = None;
    let (used, result) = if compaction_task_id.is_some() {
        let mut observer = crate::model_call::Silent;
        call_chain(&runtime, &conversation_id, &attempts, cancel, &mut observer).await
    } else {
        let (events, mut events_rx) = mpsc::unbounded_channel();
        let forwarder = {
            let runtime = runtime.clone();
            let conversation_id = conversation_id.clone();
            tokio::spawn(async move {
                while let Some(batch) = events_rx.recv().await {
                    runtime.emit(&conversation_id, batch).await;
                }
            })
        };
        let live = LiveItem::new(events);
        *projection
            .item_id
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(live.item_id.clone());
        let pi = PiObserver::new(model.clone(), options.reasoning, stream.clone());
        let mut observer = FanoutObserver::new(live, pi.clone());
        let result = call_chain(&runtime, &conversation_id, &attempts, cancel, &mut observer).await;
        partial = Some(pi.partial());
        drop(observer);
        let _ = forwarder.await;
        result
    };
    if compaction_task_id.is_none() {
        {
            let mut metadata = projection
                .metadata
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            metadata.completed = result.is_ok();
            metadata.stop_reason = result.as_ref().ok().map(|output| output.stop.clone());
        }
        if let Err(error) = &result {
            *projection
                .end
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) =
                Some(if *error == AgentError::Cancelled {
                    RunEnd::Aborted
                } else {
                    RunEnd::Failed(error.to_string())
                });
        }
    }
    match result {
        Ok(output) => {
            let used_model = attempts
                .get(used)
                .map(|request| model_catalog::pi_model(&request.model))
                .unwrap_or_else(|| model.clone());
            if let (Some(task_id), Some(request)) = (compaction_task_id, attempts.get(used)) {
                projection
                    .extra_usage
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .entry(task_id)
                    .or_default()
                    .push(Entry::ExtraUsage {
                        provider: request.model.provider.clone(),
                        model_id: request.model.id.clone(),
                        usage: output.usage,
                    });
            }
            let content = super::message::to_durable_message(&LlmMessage::Assistant {
                blocks: output.blocks.clone(),
                provider_data: None,
            });
            match content {
                Ok(rpi_ai::types::Message::Assistant(converted)) => {
                    let mut message = assistant_message(
                        &used_model,
                        converted.content,
                        output.usage,
                        output.stop.clone(),
                    );
                    message.thinking_level = attempts
                        .get(used)
                        .and_then(|request| pi_thinking_level(&request.thinking));
                    stream.push(StreamEvent::Done {
                        reason: done_reason(message.stop_reason),
                        message,
                    });
                }
                Ok(_) => stream.push(StreamEvent::Error {
                    reason: ErrorReason::Error,
                    error: error_message(
                        &model,
                        "assistant conversion returned another message type".to_owned(),
                    ),
                }),
                Err(error) => stream.push(StreamEvent::Error {
                    reason: ErrorReason::Error,
                    error: error_message(&model, error.to_string()),
                }),
            }
        }
        Err(error) => {
            let used_model = attempts
                .get(used)
                .map(|request| model_catalog::pi_model(&request.model))
                .unwrap_or_else(|| model.clone());
            let mut message =
                partial.unwrap_or_else(|| error_message(&used_model, error.to_string()));
            message.api = used_model.api;
            message.provider = used_model.provider;
            message.model = used_model.id;
            message.stop_reason = if error == AgentError::Cancelled {
                rpi_ai::types::StopReason::Aborted
            } else {
                rpi_ai::types::StopReason::Error
            };
            message.error_message = Some(error.to_string());
            stream.push(StreamEvent::Error {
                reason: if error == AgentError::Cancelled {
                    ErrorReason::Aborted
                } else {
                    ErrorReason::Error
                },
                error: message,
            });
        }
    }
}

fn done_reason(reason: rpi_ai::types::StopReason) -> DoneReason {
    match reason {
        rpi_ai::types::StopReason::ToolUse => DoneReason::ToolUse,
        rpi_ai::types::StopReason::Length => DoneReason::Length,
        _ => DoneReason::Stop,
    }
}
