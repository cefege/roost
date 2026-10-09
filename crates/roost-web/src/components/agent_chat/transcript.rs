//! Transcript projection: user and assistant messages plus tool cards.
//! Markdown safety stays in `markdown`; the fold and stream are client-core's.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentRunState, Transcript, TranscriptBlock, TranscriptItem,
};

use crate::components::md::{Card, CardVariant, Surface};

#[component]
pub fn AgentTranscriptView(transcript: Transcript) -> Element {
    rsx! {
        div { class: "agent-chat__items",
            for item in transcript.items.iter() {
                match item {
                    TranscriptItem::User { id, text } => rsx! {
                        div { key: "{id}", class: "agent-chat__user-row",
                            Surface { class: Some("agent-chat__user-bubble".to_string()), {text.clone()} }
                        }
                    },
                    TranscriptItem::Assistant { id, blocks, streaming: _, error } => rsx! {
                        div { key: "{id}", class: "agent-chat__assistant",
                            for (index, block) in blocks.iter().enumerate() {
                                match block {
                                    TranscriptBlock::Text { text } => rsx! {
                                        div { key: "text-{index}", class: "agent-chat__markdown",
                                            dangerous_inner_html: super::markdown::markdown_to_safe_html(text)
                                        }
                                    },
                                    TranscriptBlock::Thinking { text } => rsx! {
                                        details { key: "thinking-{index}", class: "agent-chat__thinking",
                                            summary { "Thinking" }
                                            pre { {text.clone()} }
                                        }
                                    },
                                    TranscriptBlock::ToolCall { .. } => rsx! {},
                                }
                            }
                            if let Some(error) = error {
                                Card { variant: CardVariant::Outlined, class: Some("agent-chat__error".to_string()), {error.clone()} }
                            }
                        }
                    },
                    TranscriptItem::Tool { id, call_id: _, tool_name, args_json, output, is_error, running } => rsx! {
                        super::tool_card::ToolCard {
                            key: "{id}", tool_name: tool_name.clone(), args_json: args_json.clone(),
                            output: output.clone(), is_error: *is_error, running: *running,
                        }
                    },
                }
            }
            if transcript.run_state == AgentRunState::Failed {
                Card {
                    variant: CardVariant::Outlined,
                    class: Some("agent-chat__error".to_string()),
                    {transcript.error.clone().unwrap_or_else(|| "The agent run failed.".to_string())}
                }
            }
        }
    }
}
