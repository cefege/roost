//! The agent workspace's semantic transcript: labelled messages, compact tool
//! disclosures and native interactive cards. Snapshot and streaming state
//! belong to client-core; this module only renders them. Queued input lives
//! beside the composer so it stays visible while reading older messages.

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentRunState, Transcript, TranscriptBlock, TranscriptItem,
};

use super::advisory_card::AdvisoryCard;
use super::agent_alert::AgentAlert;
use super::background_job_box::BackgroundJobBox;
use super::interaction_cards::{AskCard, BtwCard};
use super::message_actions::{EditTarget, UserMessageActions};
use super::notice_card::NoticeCard;
use super::peer_message_card::PeerMessageCard;
use super::plan_card::PlanCard;
use super::streaming_reveal::StreamingText;
use super::thinking_block::ThinkingBlock;
use super::timeline_rail::StepGroup;
use super::toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
use crate::components::ui::{
    Badge, BadgeSize, BadgeVariant, Bubble, BubbleContent, BubbleVariant, Button, ButtonSize,
    ButtonVariant, IconType, Marker, Message, MessageAlign, MessageContent, MessageFooter,
    StatusDot,
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
    PeerMessage(&'transcript TranscriptItem),
}

#[component]
pub fn AgentTranscriptView(
    transcript: Transcript,
    conversation_id: String,
    pump: Pump,
    draft: Signal<String>,
    edit_target: Signal<Option<EditTarget>>,
) -> Element {
    let segments = segments(&transcript);
    let has_tools = transcript
        .items
        .iter()
        .any(|item| matches!(item, TranscriptItem::Tool { .. }));
    let has_thinking = transcript.items.iter().any(|item| matches!(
        item,
        TranscriptItem::Assistant { blocks, .. }
            if blocks.iter().any(|block| matches!(block, TranscriptBlock::Thinking { text } if !text.is_empty()))
    ));
    let show_tools = use_signal(|| true);
    let show_thinking = use_signal(|| true);
    let expand_tools = use_signal(|| false);
    let running = transcript.run_state == AgentRunState::Running;
    let mut filters_placed = false;
    rsx! {
        div { class: "flex w-full max-w-full flex-col gap-6",
            for segment in segments {
                match segment {
                    Segment::User(TranscriptItem::User { id, text, attachment_names }) => {
                        let show_filters = !filters_placed && (has_tools || has_thinking);
                        filters_placed |= show_filters;
                        rsx! {
                            UserMessage {
                                key: "{id}",
                                id: id.clone(),
                                text: text.clone(),
                                attachment_names: attachment_names.clone(),
                                conversation_id: conversation_id.clone(),
                                draft,
                                edit_target,
                                running,
                                pump: pump.clone(),
                                show_filters,
                                has_tools,
                                has_thinking,
                                show_tools,
                                show_thinking,
                                expand_tools,
                            }
                        }
                    },
                    Segment::Assistant(TranscriptItem::Assistant { id, blocks, streaming, error }) => rsx! {
                        AssistantMessage { key: "{id}", blocks: blocks.clone(), streaming: *streaming, error: error.clone(), show_thinking: show_thinking() }
                    },
                    Segment::Steps(_) if !show_tools() => rsx! {},
                    Segment::Steps(steps) => {
                        let key = step_group_key(&steps);
                        let steps = steps.into_iter().cloned().collect::<Vec<_>>();
                        rsx! {
                            StepGroup { key: "{key}", steps, expanded: expand_tools() }
                        }
                    }
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
                    Segment::PeerMessage(TranscriptItem::PeerMessage { id, from, broadcast, text, .. }) => rsx! {
                        PeerMessageCard { key: "{id}", id: id.clone(), from: from.clone(), broadcast: *broadcast, text: text.clone() }
                    },
                    _ => rsx! {},
                }
            }
            if transcript.run_state == AgentRunState::Failed {
                AgentAlert {
                    p { {transcript.error.clone().unwrap_or_else(|| "The agent stopped with an error.".to_string())} }
                }
            }
        }
    }
}

#[component]
fn UserMessage(
    id: String,
    text: String,
    attachment_names: Vec<String>,
    conversation_id: String,
    draft: Signal<String>,
    edit_target: Signal<Option<EditTarget>>,
    running: bool,
    pump: Pump,
    show_filters: bool,
    has_tools: bool,
    has_thinking: bool,
    mut show_tools: Signal<bool>,
    mut show_thinking: Signal<bool>,
    mut expand_tools: Signal<bool>,
) -> Element {
    rsx! {
        Message { align: MessageAlign::Start, class: "w-full max-w-full",
            MessageContent {
                Bubble { variant: BubbleVariant::Muted, class: "w-full max-w-full",
                    BubbleContent { class: "w-full max-w-full whitespace-pre-wrap", "{text}"
                        if !attachment_names.is_empty() {
                            div { class: "mt-2 flex flex-wrap gap-1",
                                for filename in &attachment_names {
                                    Badge {
                                        variant: BadgeVariant::Outline,
                                        size: BadgeSize::Sm,
                                        class: Some("min-w-0 max-w-full".to_owned()),
                                        span { class: "block max-w-full truncate", title: "{filename}", "{filename}" }
                                    }
                                }
                            }
                        }
                    }
                }
                MessageFooter {
                    if show_filters {
                        ToolbarMenu {
                            menu_key: "transcript",
                            trigger: ToolbarTrigger::Icon { icon: IconType::ListFilter },
                            aria_label: "Transcript display",
                            items: {
                                let mut items = Vec::new();
                                if has_thinking {
                                    items.push(ToolbarMenuItem::choice("thinking", "Show thinking", show_thinking()));
                                }
                                if has_tools {
                                    items.push(ToolbarMenuItem::choice("tools", "Show tool activity", show_tools()));
                                    items.push(ToolbarMenuItem::choice("expand", "Expand tool output", expand_tools()));
                                }
                                items
                            },
                            on_choose: move |choice: String| match choice.as_str() {
                                "thinking" => show_thinking.set(!show_thinking()),
                                "tools" => show_tools.set(!show_tools()),
                                "expand" => expand_tools.set(!expand_tools()),
                                _ => {}
                            },
                        }
                    }
                    UserMessageActions { conversation_id, item_id: id, text, attachment_names, draft, edit_target, running, pump }
                }
            }
        }
    }
}

/// An assistant reply sits on the page, not in a bubble: prose at reading width,
/// thinking blocks inline, and one quiet copy action beneath.
#[component]
fn AssistantMessage(
    blocks: Vec<TranscriptBlock>,
    streaming: bool,
    error: Option<String>,
    show_thinking: bool,
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
    let (copy_response, response_copied) =
        crate::components::hooks::use_copy_clipboard::use_copy_clipboard(None);
    let has_text = !copy_text.trim().is_empty();
    rsx! {
        div { class: "group/assistant flex min-w-0 max-w-reading flex-col gap-3 text-sm text-foreground", "data-streaming": streaming.then_some("true"),
            for (index, block) in blocks.iter().enumerate() {
                match block {
                    // Every text block goes through StreamingText under one key,
                    // so a block that stops streaming keeps revealing its backlog.
                    TranscriptBlock::Text { text } if !text.is_empty() => rsx! {
                        StreamingText { key: "text-{index}", text: text.clone(), live: streaming && index + 1 == blocks.len() }
                    },
                    TranscriptBlock::Thinking { text } if !text.is_empty() => {
                        let live = streaming && Some(index) == last_thinking && index + 1 == blocks.len();
                        rsx! {
                            ThinkingBlock { key: "thinking-{index}", text: text.clone(), live, globally_visible: show_thinking }
                        }
                    }
                    _ => rsx! {},
                }
            }
            if streaming {
                Marker { role: "status",
                    StatusDot { status: "running".to_string() }
                    "Generating response"
                }
            }
            if let Some(error) = error {
                AgentAlert { p { "{error}" } }
            }
            if !streaming && has_text {
                div { class: "flex items-center text-muted-foreground",
                    Button {
                        variant: ButtonVariant::Ghost,
                        size: ButtonSize::Sm,
                        onclick: move |_| copy_response(&copy_text),
                        if response_copied() { "Copied" } else { "Copy" }
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
            TranscriptItem::Notice { .. } => segments.push(Segment::Notice(item)),
            TranscriptItem::Plan { .. } => segments.push(Segment::Plan(item)),
            TranscriptItem::Goal { .. }
            | TranscriptItem::Todo { .. }
            | TranscriptItem::LoopStatus { .. } => {}
            TranscriptItem::Ask { .. } => segments.push(Segment::Ask(item)),
            TranscriptItem::Btw { .. } => segments.push(Segment::Btw(item)),
            TranscriptItem::Advisory { .. } => segments.push(Segment::Advisory(item)),
            TranscriptItem::BackgroundJob { .. } => segments.push(Segment::BackgroundJob(item)),
            TranscriptItem::PeerMessage { .. } => segments.push(Segment::PeerMessage(item)),
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
