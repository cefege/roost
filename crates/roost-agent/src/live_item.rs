//! The live transcript item of one assistant response: provider stream events
//! become transcript events as they arrive. Driven by the durable model adapter;
//! durable partials and terminal records remain the kernel's responsibility.

use roost_llm::{AssistantBlock, BlockKind, StreamEvent};
use roost_protocol::wire::agent_chat::{ChatEvent, TranscriptBlock, TranscriptItem};
use tokio::sync::mpsc;

use crate::model_call::StreamObserver;
use crate::projection::transcript_blocks;
use crate::runtime::random_id;

/// Streams a conversation's assistant response into its live transcript
/// item. Events go through a channel to one forwarding task, which keeps
/// their order without blocking the stream on delivery.
pub(crate) struct LiveItem {
    events: mpsc::UnboundedSender<Vec<ChatEvent>>,
    pub item_id: String,
    /// Provider block index → transcript block index.
    positions: Vec<(usize, usize)>,
    /// Set once any event reached the transcript. A call that has committed
    /// visible output is never failed over to another model.
    committed: bool,
}

impl LiveItem {
    pub(crate) fn new(events: mpsc::UnboundedSender<Vec<ChatEvent>>) -> Self {
        let mut item = Self {
            events,
            item_id: random_id("a-"),
            positions: Vec::new(),
            committed: false,
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
        let position = self.positions.len();
        self.positions.push((index, position));
        position
    }
}

impl StreamObserver for LiveItem {
    fn committed(&self) -> bool {
        self.committed
    }

    fn restart(&mut self) {
        self.positions.clear();
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
        if is_substantive(event) {
            self.committed = true;
        }
        let item_id = self.item_id.clone();
        match event {
            StreamEvent::BlockStart { index, kind } => {
                let position = self.position(*index);
                let value = match kind {
                    BlockKind::Thinking => Some(TranscriptBlock::Thinking {
                        text: String::new(),
                    }),
                    BlockKind::Text => Some(TranscriptBlock::Text {
                        text: String::new(),
                    }),
                    BlockKind::ToolCall => None,
                };
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

/// Whether an event carries output a user could have seen or that a provider
/// would have to replay. Block scaffolding and empty deltas do not count, so a
/// response that failed before producing anything stays eligible for failover.
fn is_substantive(event: &StreamEvent) -> bool {
    match event {
        StreamEvent::TextDelta { text, .. } | StreamEvent::ThinkingDelta { text, .. } => {
            !text.is_empty()
        }
        StreamEvent::ToolCallStart { name, .. } => !name.is_empty(),
        StreamEvent::BlockEnd { block, .. } => match block {
            AssistantBlock::Text { text, .. } | AssistantBlock::Thinking { text, .. } => {
                !text.is_empty()
            }
            AssistantBlock::ToolCall { name, .. } => !name.is_empty(),
        },
        StreamEvent::BlockStart { .. }
        | StreamEvent::ToolArgsDelta { .. }
        | StreamEvent::Usage(_)
        | StreamEvent::Stop(_) => false,
    }
}
