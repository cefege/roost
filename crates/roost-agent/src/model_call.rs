//! One model call with the harness's failure policy: rotate accounts on a
//! rate limit (at most 3 times), retry transient provider failures with
//! backoff, and stream events into the conversation's live assistant item.

use futures::StreamExt;
use roost_llm::{AssistantBlock, BlockKind, ChatRequest, LlmError, StopReason, StreamEvent, Usage};
use roost_protocol::wire::agent_chat::{ChatEvent, TranscriptBlock, TranscriptItem};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::error::AgentError;
use crate::projection::transcript_blocks;
use crate::records::{Entry, model_ref};
use crate::runtime::{AgentRuntime, random_id};

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
                } => Some((call_id.clone(), name.clone(), args_json.clone())),
                _ => None,
            })
            .collect()
    }

    pub fn text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|block| match block {
                AssistantBlock::Text { text } => Some(text.as_str()),
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
}

/// Discards events: judgments, compaction and advisor reviews.
pub(crate) struct Silent;

impl StreamObserver for Silent {
    fn restart(&mut self) {}
    fn event(&mut self, _event: &StreamEvent) {}
}

/// Calls the model for `account_key` (the conversation whose accounts and
/// sticky selection apply) with rotation and retries.
pub(crate) async fn call_model(
    runtime: &AgentRuntime,
    account_key: &str,
    request: &ChatRequest,
    cancel: &CancellationToken,
    observer: &mut dyn StreamObserver,
) -> Result<ModelOutput, AgentError> {
    let llm = &runtime.inner.llm;
    let provider = request.model.provider.clone();
    let (mut credential_id, mut auth) = llm.resolve(&provider, account_key).await?;
    let mut rotations = 0;
    let mut transient_retries = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(AgentError::Cancelled);
        }
        let stream = llm.stream(credential_id, request.clone(), auth.clone(), cancel.clone());
        match collect(stream, cancel, observer).await {
            Ok(output) => return Ok(output),
            Err(error @ LlmError::RateLimited { .. }) if rotations < MAX_ROTATIONS => {
                rotations += 1;
                tracing::info!(
                    account_key,
                    provider,
                    rotations,
                    "rotating account after rate limit"
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
                    () = cancel.cancelled() => return Err(AgentError::Cancelled),
                    () = tokio::time::sleep(wait) => {}
                }
            }
            Err(error) => return Err(error.into()),
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

/// Streams a conversation's assistant response into its live transcript
/// item. Events go through a channel to one forwarding task, which keeps
/// their order without blocking the stream on delivery.
pub(crate) struct LiveItem {
    events: mpsc::UnboundedSender<Vec<ChatEvent>>,
    pub item_id: String,
    /// Provider block index → transcript block index.
    positions: Vec<(usize, usize)>,
    /// Partial blocks, kept so an aborted response still records its text.
    partial: Vec<AssistantBlock>,
}

impl LiveItem {
    fn new(events: mpsc::UnboundedSender<Vec<ChatEvent>>) -> Self {
        let mut item = Self {
            events,
            item_id: random_id("a-"),
            positions: Vec::new(),
            partial: Vec::new(),
        };
        item.restart();
        item
    }

    fn send(&self, event: ChatEvent) {
        let _ = self.events.send(vec![event]);
    }

    fn position(&mut self, index: usize) -> usize {
        if let Some((_, position)) = self.positions.iter().find(|(known, _)| *known == index) {
            return *position;
        }
        let position = self.partial.len();
        self.positions.push((index, position));
        self.partial.push(AssistantBlock::Text {
            text: String::new(),
        });
        position
    }

    fn partial_text(&self) -> Vec<AssistantBlock> {
        self.partial
            .iter()
            .filter(|block| match block {
                AssistantBlock::Text { text } | AssistantBlock::Thinking { text, .. } => {
                    !text.is_empty()
                }
                AssistantBlock::ToolCall { .. } => false,
            })
            .cloned()
            .collect()
    }
}

impl StreamObserver for LiveItem {
    fn restart(&mut self) {
        self.positions.clear();
        self.partial.clear();
        self.send(ChatEvent::Item {
            item: TranscriptItem::Assistant {
                id: self.item_id.clone(),
                blocks: Vec::new(),
                streaming: true,
                error: None,
            },
        });
    }

    fn event(&mut self, event: &StreamEvent) {
        let item_id = self.item_id.clone();
        match event {
            StreamEvent::BlockStart { index, kind } => {
                let position = self.position(*index);
                let (block, value) = match kind {
                    BlockKind::Thinking => (
                        AssistantBlock::Thinking {
                            text: String::new(),
                            provider_data: None,
                        },
                        Some(TranscriptBlock::Thinking {
                            text: String::new(),
                        }),
                    ),
                    BlockKind::Text => (
                        AssistantBlock::Text {
                            text: String::new(),
                        },
                        Some(TranscriptBlock::Text {
                            text: String::new(),
                        }),
                    ),
                    BlockKind::ToolCall => (
                        AssistantBlock::Text {
                            text: String::new(),
                        },
                        None,
                    ),
                };
                self.partial[position] = block;
                if let Some(value) = value {
                    self.send(ChatEvent::BlockSet {
                        item_id,
                        block: position,
                        value,
                    });
                }
            }
            StreamEvent::TextDelta { index, text } | StreamEvent::ThinkingDelta { index, text } => {
                let position = self.position(*index);
                if let AssistantBlock::Text { text: buffer }
                | AssistantBlock::Thinking { text: buffer, .. } = &mut self.partial[position]
                {
                    buffer.push_str(text);
                }
                let delta = text.clone();
                self.send(if matches!(event, StreamEvent::ThinkingDelta { .. }) {
                    ChatEvent::ThinkingDelta {
                        item_id,
                        block: position,
                        delta,
                    }
                } else {
                    ChatEvent::TextDelta {
                        item_id,
                        block: position,
                        delta,
                    }
                });
            }
            StreamEvent::ToolCallStart {
                index,
                call_id,
                name,
            } => {
                let position = self.position(*index);
                self.partial[position] = AssistantBlock::ToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    args_json: String::new(),
                };
                self.send(ChatEvent::BlockSet {
                    item_id,
                    block: position,
                    value: TranscriptBlock::ToolCall {
                        call_id: call_id.clone(),
                        tool_name: name.clone(),
                        args_json: String::new(),
                    },
                });
            }
            StreamEvent::BlockEnd { index, block } => {
                let position = self.position(*index);
                self.partial[position] = block.clone();
                if let Some(value) = transcript_blocks(std::slice::from_ref(block)).pop() {
                    self.send(ChatEvent::BlockSet {
                        item_id,
                        block: position,
                        value,
                    });
                }
            }
            StreamEvent::ToolArgsDelta { .. } | StreamEvent::Usage(_) | StreamEvent::Stop(_) => {}
        }
    }
}

/// The primary call of a turn: streams into the transcript and records the
/// assistant entry. An aborted response records whatever text had arrived.
pub(crate) async fn primary_call(
    runtime: &AgentRuntime,
    conversation_id: &str,
    request: &ChatRequest,
    cancel: &CancellationToken,
) -> Result<(u64, ModelOutput), AgentError> {
    let (events, mut events_rx) = mpsc::unbounded_channel::<Vec<ChatEvent>>();
    let forwarder = {
        let runtime = runtime.clone();
        let conversation_id = conversation_id.to_owned();
        tokio::spawn(async move {
            while let Some(batch) = events_rx.recv().await {
                runtime.emit(&conversation_id, batch).await;
            }
        })
    };
    let mut live = LiveItem::new(events);
    let result = call_model(runtime, conversation_id, request, cancel, &mut live).await;
    let item_id = live.item_id.clone();
    let partial = live.partial_text();
    drop(live);
    let _ = forwarder.await;
    let reference = model_ref(&request.model.provider, &request.model.id);
    let (blocks, usage) = match &result {
        Ok(output) => (output.blocks.clone(), output.usage),
        Err(_) => (partial, Usage::default()),
    };
    let entry = Entry::Assistant {
        item_id: item_id.clone(),
        blocks: blocks.clone(),
        usage,
        model: Some(reference),
    };
    let seq = runtime
        .inner
        .store
        .append_entry(conversation_id, &entry)
        .await?;
    let error_text = result.as_ref().err().and_then(|error| match error {
        AgentError::Cancelled => None,
        other => Some(other.to_string()),
    });
    runtime
        .emit(
            conversation_id,
            vec![ChatEvent::Item {
                item: TranscriptItem::Assistant {
                    id: item_id,
                    blocks: transcript_blocks(&blocks),
                    streaming: false,
                    error: error_text,
                },
            }],
        )
        .await;
    if let Ok(totals) = runtime.usage_totals(conversation_id).await {
        runtime
            .emit(conversation_id, vec![ChatEvent::Usage { usage: totals }])
            .await;
    }
    result.map(|output| (seq, output))
}
