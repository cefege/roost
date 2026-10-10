//! The transcript: user messages as soft bubbles, assistant replies as
//! full-width prose with a Copy action, and each run of consecutive tool calls
//! as one step timeline. The fold and the stream are client-core's; Markdown
//! safety is `markdown`'s. Rendered by `agent_chat::surface`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentRunState, Transcript, TranscriptBlock, TranscriptItem,
};

use super::tool_card::ToolCard;
use crate::components::md::{ButtonVariant, Icon, IconButton, IconButtonSize, IconSize};

/// A stretch of the transcript drawn as one unit.
enum Segment<'transcript> {
    User(&'transcript TranscriptItem),
    Assistant(&'transcript TranscriptItem),
    Steps(Vec<&'transcript TranscriptItem>),
}

#[component]
pub fn AgentTranscriptView(transcript: Transcript) -> Element {
    let segments = segments(&transcript);
    let streaming = assistant_is_streaming(&transcript);
    let running = transcript.run_state == AgentRunState::Running;
    rsx! {
        div { class: "agent-chat__items",
            for segment in segments {
                match segment {
                    Segment::User(TranscriptItem::User { id, text }) => rsx! {
                        UserMessage { key: "{id}", text: text.clone() }
                    },
                    Segment::Assistant(TranscriptItem::Assistant { id, blocks, streaming, error }) => rsx! {
                        AssistantMessage { key: "{id}", blocks: blocks.clone(), streaming: *streaming, error: error.clone() }
                    },
                    Segment::Steps(steps) => rsx! {
                        div { key: "{step_group_key(&steps)}", class: "agent-chat__steps",
                            for step in steps {
                                if let TranscriptItem::Tool { id, tool_name, args_json, output, is_error, running, .. } = step {
                                    ToolCard {
                                        key: "{id}",
                                        tool_name: tool_name.clone(),
                                        args_json: args_json.clone(),
                                        output: output.clone(),
                                        is_error: *is_error,
                                        running: *running,
                                    }
                                }
                            }
                        }
                    },
                    _ => rsx! {},
                }
            }
            if running && !streaming {
                div { class: "agent-chat__working", role: "status",
                    span { class: "agent-chat__working-mark" }
                    span { class: "agent-chat__shimmer", "Working…" }
                }
            }
            if transcript.run_state == AgentRunState::Failed {
                div { class: "agent-chat__notice", "data-tone": "error", role: "alert",
                    Icon { name: "error", size: IconSize::Sm }
                    span { {transcript.error.clone().unwrap_or_else(|| "The agent stopped with an error.".to_string())} }
                }
            }
        }
    }
}

#[component]
fn UserMessage(text: String) -> Element {
    rsx! {
        div { class: "agent-chat__user",
            div { class: "agent-chat__user-bubble", "{text}" }
        }
    }
}

#[component]
fn AssistantMessage(
    blocks: Vec<TranscriptBlock>,
    streaming: bool,
    error: Option<String>,
) -> Element {
    let last_thinking = blocks
        .iter()
        .rposition(|block| matches!(block, TranscriptBlock::Thinking { .. }));
    let copy_text = blocks
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let has_text = !copy_text.trim().is_empty();
    rsx! {
        div { class: "agent-chat__assistant", "data-streaming": streaming.then_some("true"),
            for (index, block) in blocks.iter().enumerate() {
                match block {
                    TranscriptBlock::Text { text } if !text.is_empty() => rsx! {
                        div {
                            key: "text-{index}",
                            class: "agent-chat__markdown",
                            dangerous_inner_html: super::markdown::markdown_to_safe_html(text),
                        }
                    },
                    TranscriptBlock::Thinking { text } => {
                        let live = streaming && Some(index) == last_thinking && index + 1 == blocks.len();
                        rsx! {
                            details { key: "thinking-{index}", class: "agent-chat__thinking",
                                summary {
                                    Icon { name: "psychology", size: IconSize::Sm }
                                    span { class: if live { "agent-chat__shimmer" } else { "" }, if live { "Thinking…" } else { "Thought process" } }
                                    span { class: "agent-chat__thinking-chevron", Icon { name: "expand_more", size: IconSize::Sm } }
                                }
                                div { class: "agent-chat__thinking-text", "{text}" }
                            }
                        }
                    }
                    _ => rsx! {},
                }
            }
            if let Some(error) = error {
                div { class: "agent-chat__notice", "data-tone": "error", role: "alert",
                    Icon { name: "error", size: IconSize::Sm }
                    span { "{error}" }
                }
            }
            if has_text && !streaming {
                div { class: "agent-chat__message-actions",
                    IconButton {
                        icon: "content_copy",
                        label: "Copy response",
                        title: "Copy",
                        variant: ButtonVariant::Ghost,
                        size: IconButtonSize::IconSm,
                        onclick: move |_| {
                            crate::components::notifications::clipboard::copy_text(&copy_text);
                        },
                    }
                }
            }
        }
    }
}

/// Fold the items into segments: each user and assistant item alone, and
/// every run of consecutive tool items as one step group.
fn segments(transcript: &Transcript) -> Vec<Segment<'_>> {
    let mut segments: Vec<Segment<'_>> = Vec::new();
    for item in &transcript.items {
        match item {
            TranscriptItem::User { .. } => segments.push(Segment::User(item)),
            TranscriptItem::Assistant {
                blocks,
                error,
                streaming,
                ..
            } => {
                // A turn that only called tools has nothing to show of its own.
                let visible = *streaming
                    || error.is_some()
                    || blocks.iter().any(|block| match block {
                        TranscriptBlock::Text { text } | TranscriptBlock::Thinking { text } => {
                            !text.is_empty()
                        }
                        TranscriptBlock::ToolCall { .. } => false,
                    });
                if visible {
                    segments.push(Segment::Assistant(item));
                }
            }
            TranscriptItem::Tool { .. } => match segments.last_mut() {
                Some(Segment::Steps(steps)) => steps.push(item),
                _ => segments.push(Segment::Steps(vec![item])),
            },
        }
    }
    segments
}

fn step_group_key(steps: &[&TranscriptItem]) -> String {
    match steps.first() {
        Some(TranscriptItem::Tool { id, .. }) => format!("steps-{id}"),
        _ => "steps".to_owned(),
    }
}

/// Whether the newest item is an assistant message still receiving text: it
/// is then its own progress signal, and a second one would be noise.
fn assistant_is_streaming(transcript: &Transcript) -> bool {
    matches!(
        transcript.items.last(),
        Some(TranscriptItem::Assistant {
            streaming: true,
            ..
        })
    )
}
