//! The transcript, laid out as OMP's terminal transcript: user messages as
//! `Surface` bubbles, assistant replies as full-width prose (the reply being
//! written paced by `streaming_reveal`), and tool calls inline with their
//! output folded to each tool's preview window. "Expand tool output" opens
//! every call at once. The fold and the stream are client-core's; Markdown
//! safety is `markdown`'s. Rendered by `agent_chat::surface`.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentRunState, Transcript, TranscriptBlock, TranscriptItem,
};

use super::advisory_card::AdvisoryCard;
use super::agent_alert::AgentAlert;
use super::background_job_box::BackgroundJobBox;
use super::interaction_cards::{AskCard, BtwCard};
use super::message_actions::{ForkMessageAction, UserMessageActions};
use super::notice_card::NoticeCard;
use super::plan_card::PlanCard;
use super::queued_band::QueuedBand;
use super::streaming_reveal::StreamingText;
use super::thinking_block::ThinkingBlock;
use super::tool_card::ToolCard;
use super::working_row::{WorkingRow, working_label};
use crate::components::md::{
    Button, ButtonVariant, Icon, IconButton, IconButtonSize, IconSize, StatusDot, Surface,
    SurfaceRadius,
};
use crate::pump::Pump;

enum Segment<'transcript> {
    User(&'transcript TranscriptItem),
    Assistant(&'transcript TranscriptItem),
    Steps(Vec<&'transcript TranscriptItem>),
    Notice(&'transcript TranscriptItem),
    Plan(&'transcript TranscriptItem),
    Advisory(&'transcript TranscriptItem),
    Ask(&'transcript TranscriptItem),
    Btw(&'transcript TranscriptItem),
    BackgroundJob(&'transcript TranscriptItem),
}

#[component]
pub fn AgentTranscriptView(
    transcript: Transcript,
    conversation_id: String,
    pump: Pump,
    draft: Signal<String>,
) -> Element {
    let segments = segments(&transcript);
    let streaming = assistant_is_streaming(&transcript);
    let running = transcript.run_state == AgentRunState::Running;
    let has_tools = transcript
        .items
        .iter()
        .any(|item| matches!(item, TranscriptItem::Tool { .. }));
    let has_thinking = transcript.items.iter().any(|item| matches!(
        item,
        TranscriptItem::Assistant { blocks, .. }
            if blocks.iter().any(|block| matches!(block, TranscriptBlock::Thinking { text } if !text.is_empty()))
    ));
    let mut show_tools = use_signal(|| true);
    let mut show_thinking = use_signal(|| true);
    let mut expand_tools = use_signal(|| false);
    rsx! {
        div { class: "agent-chat__items",
            if has_tools || has_thinking {
                div { class: "agent-chat__transcript-controls",
                    if has_thinking {
                        Button {
                            variant: ButtonVariant::Link,
                            aria_expanded: show_thinking().to_string(),
                            onclick: move |_| show_thinking.set(!show_thinking()),
                            Icon { name: if show_thinking() { "visibility" } else { "visibility_off" }, size: IconSize::Sm }
                            if show_thinking() { "Hide thinking" } else { "Show thinking" }
                        }
                    }
                    if has_tools {
                        Button {
                            variant: ButtonVariant::Link,
                            aria_expanded: show_tools().to_string(),
                            onclick: move |_| show_tools.set(!show_tools()),
                            Icon { name: if show_tools() { "visibility" } else { "visibility_off" }, size: IconSize::Sm }
                            if show_tools() { "Hide tool activity" } else { "Show tool activity" }
                        }
                        if show_tools() {
                            Button {
                                variant: ButtonVariant::Link,
                                aria_pressed: expand_tools().to_string(),
                                "data-testid": "agent-chat-expand-tools",
                                onclick: move |_| expand_tools.set(!expand_tools()),
                                Icon { name: if expand_tools() { "unfold_less" } else { "unfold_more" }, size: IconSize::Sm }
                                if expand_tools() { "Fold tool output" } else { "Expand tool output" }
                            }
                        }
                    }
                }
            }
            for segment in segments {
                match segment {
                    Segment::User(TranscriptItem::User { id, text }) => rsx! {
                        UserMessage { key: "{id}", id: id.clone(), text: text.clone(), conversation_id: conversation_id.clone(), draft, pump: pump.clone() }
                    },
                    Segment::Assistant(TranscriptItem::Assistant { id, blocks, streaming, error }) => rsx! {
                        AssistantMessage { key: "{id}", id: id.clone(), blocks: blocks.clone(), streaming: *streaming, error: error.clone(), show_thinking: show_thinking(), conversation_id: conversation_id.clone(), pump: pump.clone() }
                    },
                    Segment::Steps(_) if !show_tools() => rsx! {},
                    Segment::Steps(steps) => rsx! {
                        div { key: "{step_group_key(&steps)}", class: "agent-chat__steps",
                            for step in steps {
                                if let TranscriptItem::Tool { id, tool_name, args_json, output, is_error, running, queued, details_json, children, .. } = step {
                                    ToolCard {
                                        key: "{id}",
                                        tool_name: tool_name.clone(),
                                        args_json: args_json.clone(),
                                        output: output.clone(),
                                        is_error: *is_error,
                                        running: *running,
                                        queued: *queued,
                                        details_json: details_json.clone(),
                                        child_ids: children.clone(),
                                        expanded: expand_tools(),
                                    }
                                }
                            }
                        }
                    },
                    Segment::Notice(TranscriptItem::Notice { id, level, title, body }) => rsx! {
                        NoticeCard { key: "{id}", id: id.clone(), level: level.clone(), title: title.clone(), body: body.clone() }
                    },
                    Segment::Plan(TranscriptItem::Plan { id, title, content, state }) => rsx! {
                        PlanCard { key: "{id}", id: id.clone(), title: title.clone(), content: content.clone(), state: state.clone(), conversation_id: conversation_id.clone() }
                    },
                    Segment::Advisory(TranscriptItem::Advisory { id, severity, note, delivered }) => rsx! {
                        AdvisoryCard { key: "{id}", id: id.clone(), severity: severity.clone(), note: note.clone(), delivered: *delivered }
                    },
                    Segment::Ask(TranscriptItem::Ask { id, questions, answers }) => rsx! {
                        AskCard { key: "{id}", conversation_id: conversation_id.clone(), item_id: id.clone(), questions: questions.clone(), answers: answers.clone(), pump: pump.clone() }
                    },
                    Segment::Btw(TranscriptItem::Btw { id, question, answer, error }) => rsx! {
                        BtwCard { key: "{id}", question: question.clone(), answer: answer.clone(), error: error.clone() }
                    },
                    Segment::BackgroundJob(TranscriptItem::BackgroundJob { id, job_id, command, state, is_error, output, exit_code }) => rsx! {
                        BackgroundJobBox { key: "{id}", job_id: job_id.clone(), command: command.clone(), state: *state, is_error: *is_error, output: output.clone(), exit_code: *exit_code }
                    },
                    _ => rsx! {},
                }
            }
            if running {
                WorkingRow { label: working_label(&transcript.items), paused: streaming }
            }
            if transcript.run_state == AgentRunState::Failed {
                AgentAlert {
                    p { {transcript.error.clone().unwrap_or_else(|| "The agent stopped with an error.".to_string())} }
                }
            }
            QueuedBand { messages: transcript.queued.clone() }
        }
    }
}

/// OMP's user card: the accent washed into the panel, with an accent hairline.
const USER_BUBBLE_TINT: &str = "background: color-mix(in srgb, var(--chat-accent) 12%, var(--surface-2)); \
     border: var(--workbench-border-width) solid color-mix(in srgb, var(--chat-accent) 35%, transparent);";

#[component]
fn UserMessage(
    id: String,
    text: String,
    conversation_id: String,
    draft: Signal<String>,
    pump: Pump,
) -> Element {
    rsx! {
        div { class: "agent-chat__user",
            Surface { class: "agent-chat__user-bubble", level: 2, radius: SurfaceRadius::Md, pad: 3, style: USER_BUBBLE_TINT.to_string(), "{text}" }
            UserMessageActions { conversation_id, item_id: id, text, draft, pump }
        }
    }
}

#[component]
fn AssistantMessage(
    id: String,
    blocks: Vec<TranscriptBlock>,
    streaming: bool,
    error: Option<String>,
    show_thinking: bool,
    conversation_id: String,
    pump: Pump,
) -> Element {
    let last_thinking = blocks
        .iter()
        .rposition(|block| matches!(block, TranscriptBlock::Thinking { text } if !text.is_empty()));
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
                    // Every text block goes through StreamingText under one key,
                    // so a block that stops streaming keeps revealing its backlog.
                    TranscriptBlock::Text { text } if !text.is_empty() => rsx! {
                        StreamingText { key: "text-{index}", text: text.clone(), live: streaming && index + 1 == blocks.len() }
                    },
                    TranscriptBlock::Thinking { text } if !text.is_empty() => {
                        let live = streaming && Some(index) == last_thinking && index + 1 == blocks.len();
                        rsx! { ThinkingBlock { key: "thinking-{index}", text: text.clone(), live, globally_visible: show_thinking } }
                    }
                    _ => rsx! {},
                }
            }
            if streaming {
                div { class: "agent-chat__live-tail", role: "status", "aria-label": "Assistant response is streaming",
                    StatusDot { status: "running".to_string() }
                    "Generating response"
                }
            }
            if let Some(error) = error {
                AgentAlert { p { "{error}" } }
            }
            if !streaming {
                div { class: "agent-chat__message-actions",
                    if has_text {
                        IconButton {
                            icon: "content_copy",
                            label: "Copy response",
                            title: "Copy",
                            variant: ButtonVariant::Ghost,
                            size: IconButtonSize::Icon,
                            onclick: move |_| {
                                crate::components::notifications::clipboard::copy_text(&copy_text);
                            },
                        }
                    }
                    ForkMessageAction { conversation_id: conversation_id.clone(), item_id: id.clone(), pump: pump.clone() }
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
            TranscriptItem::Notice { .. } => segments.push(Segment::Notice(item)),
            TranscriptItem::Plan { .. } => segments.push(Segment::Plan(item)),
            TranscriptItem::Goal { .. }
            | TranscriptItem::Todo { .. }
            | TranscriptItem::LoopStatus { .. } => {}
            TranscriptItem::Ask { .. } => segments.push(Segment::Ask(item)),
            TranscriptItem::Btw { .. } => segments.push(Segment::Btw(item)),
            TranscriptItem::Advisory { .. } => segments.push(Segment::Advisory(item)),
            TranscriptItem::BackgroundJob { .. } => segments.push(Segment::BackgroundJob(item)),
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

/// Whether the newest visible item is an assistant message still receiving
/// text. Docked goal/status records are not transcript progress indicators.
fn assistant_is_streaming(transcript: &Transcript) -> bool {
    transcript
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            TranscriptItem::Goal { .. }
            | TranscriptItem::Todo { .. }
            | TranscriptItem::LoopStatus { .. } => None,
            TranscriptItem::Assistant { streaming, .. } => Some(*streaming),
            _ => Some(false),
        })
        .unwrap_or(false)
}
