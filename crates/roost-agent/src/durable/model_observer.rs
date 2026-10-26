//! Roost stream events converted to Pi partial messages and native live items.
//! The same snapshot supplies terminal errors so cancellation never replaces
//! received content and usage with an empty synthetic response.

use std::sync::{Arc, Mutex};

use roost_llm::{
    AssistantBlock, BlockKind, Message as LlmMessage, StopReason, StreamEvent as LlmEvent,
    Usage as LlmUsage,
};
use rpi_ai::types::{AssistantContent, AssistantMessage, Model, ModelThinkingLevel, StreamEvent};
use rpi_ai::utils::event_stream::AssistantMessageEventStream;
use rpi_ai::utils::json_parse::parse_streaming_json;

use crate::live_item::LiveItem;
use crate::model_call::StreamObserver;

use super::model_response::assistant_message;

enum ObserverEvent {
    Start(BlockKind),
    TextDelta(String),
    ThinkingDelta(String),
    ToolDelta(String),
    End(AssistantBlock),
    Ignore,
}

struct PartialBlock {
    source_index: usize,
    content: AssistantContent,
    arguments: String,
}

#[derive(Default)]
struct PartialOutput {
    blocks: Vec<PartialBlock>,
    usage: LlmUsage,
}

impl PartialOutput {
    fn block(&mut self, source_index: usize, kind: BlockKind) -> (usize, &mut PartialBlock) {
        let position = if let Some(position) = self
            .blocks
            .iter()
            .position(|block| block.source_index == source_index)
        {
            position
        } else {
            self.blocks.push(PartialBlock {
                source_index,
                content: match kind {
                    BlockKind::Text => AssistantContent::Text(Default::default()),
                    BlockKind::Thinking => AssistantContent::Thinking(Default::default()),
                    BlockKind::ToolCall => AssistantContent::ToolCall(Default::default()),
                },
                arguments: String::new(),
            });
            self.blocks.len() - 1
        };
        (position, &mut self.blocks[position])
    }
}

#[derive(Clone)]
pub(super) struct PiObserver {
    model: Model,
    thinking: Option<ModelThinkingLevel>,
    stream: AssistantMessageEventStream,
    output: Arc<Mutex<PartialOutput>>,
}

impl PiObserver {
    pub(super) fn new(
        model: Model,
        thinking: Option<ModelThinkingLevel>,
        stream: AssistantMessageEventStream,
    ) -> Self {
        let observer = Self {
            model,
            thinking,
            stream,
            output: Arc::new(Mutex::new(PartialOutput::default())),
        };
        observer.stream.push(StreamEvent::Start {
            partial: Arc::new(observer.partial()),
        });
        observer
    }

    pub(super) fn partial(&self) -> AssistantMessage {
        let output = self
            .output
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let mut message = assistant_message(
            &self.model,
            output
                .blocks
                .iter()
                .map(|block| block.content.clone())
                .collect(),
            output.usage,
            StopReason::EndTurn,
        );
        message.stop_reason = rpi_ai::types::StopReason::Pending;
        message.thinking_level = self.thinking;
        message
    }
}

impl StreamObserver for PiObserver {
    fn restart(&mut self) {
        *self
            .output
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = PartialOutput::default();
    }

    fn event(&mut self, event: &LlmEvent) {
        let event_kind = {
            let mut output = self
                .output
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            match event {
                LlmEvent::BlockStart { index, kind } => {
                    let (position, _) = output.block(*index, *kind);
                    Some((
                        position,
                        match kind {
                            BlockKind::ToolCall => ObserverEvent::Ignore,
                            other => ObserverEvent::Start(*other),
                        },
                    ))
                }
                LlmEvent::TextDelta { index, text } => {
                    let (position, block) = output.block(*index, BlockKind::Text);
                    if let AssistantContent::Text(block) = &mut block.content {
                        block.text.push_str(text);
                    }
                    Some((position, ObserverEvent::TextDelta(text.clone())))
                }
                LlmEvent::ThinkingDelta { index, text } => {
                    let (position, block) = output.block(*index, BlockKind::Thinking);
                    if let AssistantContent::Thinking(block) = &mut block.content {
                        block.thinking.push_str(text);
                    }
                    Some((position, ObserverEvent::ThinkingDelta(text.clone())))
                }
                LlmEvent::ToolCallStart {
                    index,
                    call_id,
                    name,
                } => {
                    let (position, block) = output.block(*index, BlockKind::ToolCall);
                    if let AssistantContent::ToolCall(call) = &mut block.content {
                        call.id = call_id.clone();
                        call.name = name.clone();
                    }
                    Some((position, ObserverEvent::Start(BlockKind::ToolCall)))
                }
                LlmEvent::ToolArgsDelta { index, json } => {
                    let (position, block) = output.block(*index, BlockKind::ToolCall);
                    block.arguments.push_str(json);
                    if let AssistantContent::ToolCall(call) = &mut block.content {
                        call.arguments = parse_streaming_json(Some(&block.arguments));
                    }
                    Some((position, ObserverEvent::ToolDelta(json.clone())))
                }
                LlmEvent::BlockEnd { index, block } => {
                    let kind = match block {
                        AssistantBlock::Text { .. } => BlockKind::Text,
                        AssistantBlock::Thinking { .. } => BlockKind::Thinking,
                        AssistantBlock::ToolCall { .. } => BlockKind::ToolCall,
                    };
                    let (position, partial) = output.block(*index, kind);
                    if let Some(finished) = durable_block(block) {
                        partial.content = finished;
                    }
                    Some((position, ObserverEvent::End(block.clone())))
                }
                LlmEvent::Usage(update) => {
                    output.usage.input = output.usage.input.max(update.input);
                    output.usage.output = output.usage.output.max(update.output);
                    output.usage.cache_read = output.usage.cache_read.max(update.cache_read);
                    output.usage.cache_write = output.usage.cache_write.max(update.cache_write);
                    None
                }
                _ => None,
            }
        };
        let Some((index, event_kind)) = event_kind else {
            return;
        };
        let partial = Arc::new(self.partial());
        let pi_event = match event_kind {
            ObserverEvent::Start(BlockKind::Text) => StreamEvent::TextStart {
                content_index: index,
                partial,
            },
            ObserverEvent::Start(BlockKind::Thinking) => StreamEvent::ThinkingStart {
                content_index: index,
                partial,
            },
            ObserverEvent::Start(BlockKind::ToolCall) => StreamEvent::ToolCallStart {
                content_index: index,
                partial,
            },
            ObserverEvent::TextDelta(delta) => StreamEvent::TextDelta {
                content_index: index,
                delta,
                partial,
            },
            ObserverEvent::ThinkingDelta(delta) => StreamEvent::ThinkingDelta {
                content_index: index,
                delta,
                partial,
            },
            ObserverEvent::ToolDelta(delta) => StreamEvent::ToolCallDelta {
                content_index: index,
                delta,
                partial,
            },
            ObserverEvent::End(AssistantBlock::Text { text, .. }) => StreamEvent::TextEnd {
                content_index: index,
                content: text,
                partial,
            },
            ObserverEvent::End(AssistantBlock::Thinking { text, .. }) => StreamEvent::ThinkingEnd {
                content_index: index,
                content: text,
                partial,
            },
            ObserverEvent::End(AssistantBlock::ToolCall { .. }) => {
                let Some(AssistantContent::ToolCall(call)) = partial.content.get(index) else {
                    return;
                };
                StreamEvent::ToolCallEnd {
                    content_index: index,
                    tool_call: call.clone(),
                    partial,
                }
            }
            ObserverEvent::Ignore => return,
        };
        self.stream.push(pi_event);
    }
}

pub(super) struct FanoutObserver {
    live: LiveItem,
    pi: PiObserver,
}

impl FanoutObserver {
    pub(super) fn new(live: LiveItem, pi: PiObserver) -> Self {
        Self { live, pi }
    }
}

impl StreamObserver for FanoutObserver {
    fn restart(&mut self) {
        self.live.restart();
        self.pi.restart();
    }
    fn event(&mut self, event: &LlmEvent) {
        self.live.event(event);
        self.pi.event(event);
    }
    fn committed(&self) -> bool {
        self.live.committed()
    }
}

fn durable_block(block: &AssistantBlock) -> Option<AssistantContent> {
    let durable = crate::durable::message::to_durable_message(&LlmMessage::Assistant {
        blocks: vec![block.clone()],
        provider_data: None,
    })
    .ok()?;
    match durable {
        rpi_ai::types::Message::Assistant(message) => message.content.into_iter().next(),
        _ => None,
    }
}
