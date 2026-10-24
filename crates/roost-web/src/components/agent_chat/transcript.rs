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
use super::ask_card::AskCard;
use super::background_job_box::BackgroundJobBox;
use super::btw_card::BtwCard;
use super::message_actions::UserMessageActions;
use super::notice_card::NoticeCard;
use super::peer_message_card::PeerMessageCard;
use super::plan_card::PlanCard;
use super::streaming_reveal::StreamingText;
use super::thinking_block::ThinkingBlock;
use super::tool_card::ToolCard;
use crate::components::ui::{Message, MessageAlign, MessageContent, MessageFooter, MessageHeader};
use crate::pump::Pump;
use libero::components::{ActionIcon, Button, Flex, Indicator, Pictogram, Text};
use libero::sx::sx;

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
    let mut show_tools = use_signal(|| true);
    let mut show_thinking = use_signal(|| true);
    let mut expand_tools = use_signal(|| false);
    let transcript_sx = use_hook(|| sx().display("flex").flex_direction("column").gap("xl"));
    let controls_sx = use_hook(|| {
        sx().display("flex")
            .flex_wrap("wrap")
            .align_items("center")
            .justify_content("flex-end")
            .gap("sm")
    });
    rsx! {
        Flex { direction: "column", sx: transcript_sx,
            if has_tools || has_thinking {
                Flex { direction: "row", sx: controls_sx,
                    if has_thinking {
                        Button {
                            variant: "standard",
                            "aria-expanded": show_thinking().to_string(),
                            onclick: move |_| show_thinking.set(!show_thinking()),
                            icon: rsx! { Pictogram { icon: if show_thinking() { pictogram_icons_lucide::eye::outlined } else { pictogram_icons_lucide::eye_off::outlined }, width: "1rem", height: "1rem" } },
                            if show_thinking() { "Hide thinking" } else { "Show thinking" }
                        }
                    }
                    if has_tools {
                        Button {
                            variant: "standard",
                            "aria-expanded": show_tools().to_string(),
                            onclick: move |_| show_tools.set(!show_tools()),
                            icon: rsx! { Pictogram { icon: if show_tools() { pictogram_icons_lucide::eye::outlined } else { pictogram_icons_lucide::eye_off::outlined }, width: "1rem", height: "1rem" } },
                            if show_tools() { "Hide tool activity" } else { "Show tool activity" }
                        }
                        if show_tools() {
                            Button {
                                variant: "standard",
                                "aria-pressed": expand_tools().to_string(),
                                "data-testid": "agent-chat-expand-tools",
                                onclick: move |_| expand_tools.set(!expand_tools()),
                                icon: rsx! { Pictogram { icon: if expand_tools() { pictogram_icons_lucide::fold_vertical::outlined } else { pictogram_icons_lucide::unfold_vertical::outlined }, width: "1rem", height: "1rem" } },
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
                        AssistantMessage { key: "{id}", blocks: blocks.clone(), streaming: *streaming, error: error.clone(), show_thinking: show_thinking() }
                    },
                    Segment::Steps(_) if !show_tools() => rsx! {},
                    Segment::Steps(steps) => rsx! {
                        Flex { key: "{step_group_key(&steps)}", direction: "column", gap: "sm",
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
    conversation_id: String,
    draft: Signal<String>,
    pump: Pump,
) -> Element {
    let user_message_sx = use_hook(|| {
        sx().align_items("flex-end")
            .selector("&[data-align='End']", sx().flex_direction("column"))
    });
    let user_header_sx = use_hook(|| sx().justify_content("flex-end"));
    let user_content_sx = use_hook(|| {
        sx().width("80%")
            .white_space("pre-wrap")
            .text_align("right")
    });
    rsx! {
        Message { align: MessageAlign::End, sx: user_message_sx,
            MessageHeader { sx: user_header_sx, "You" }
            MessageContent { sx: user_content_sx, "{text}" }
            UserMessageActions { conversation_id, item_id: id, text, draft, pump }
        }
    }
}

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
    let assistant_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("md")
            .selector(
                "&:hover [data-name='MessageFooter'], &:focus-within [data-name='MessageFooter']",
                sx().opacity("1"),
            )
            .media(
                "(pointer: coarse)",
                sx().selector("& [data-name='MessageFooter']", sx().opacity("1")),
            )
    });
    let assistant_message_sx = use_hook(|| sx().flex_direction("column").gap("md"));
    let assistant_content_sx = use_hook(|| sx().gap("sm"));
    let copy_footer_sx = use_hook(|| {
        sx().margin_top("0")
            .gap("xs")
            .padding("0")
            .opacity("0")
            .transition("opacity")
    });
    rsx! {
        Flex { sx: assistant_sx, "data-streaming": streaming.then_some("true"),
            Message { align: MessageAlign::Start, sx: assistant_message_sx,
                MessageHeader { "Agent" }
                MessageContent { sx: assistant_content_sx,
                    for (index, block) in blocks.iter().enumerate() {
                        match block {
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
                        Flex { direction: "row", align: "center", gap: "xs", role: "status",
                            Indicator { color: "info", processing: true }
                            Text { "Generating response" }
                        }
                    }
                    if let Some(error) = error {
                        AgentAlert { p { "{error}" } }
                    }
                }
                if !streaming && has_text {
                    MessageFooter { sx: copy_footer_sx,
                        ActionIcon {
                            icon: if response_copied() { pictogram_icons_lucide::check::outlined } else { pictogram_icons_lucide::copy::outlined },
                            aria_label: "Copy response",
                            title: if response_copied() { "Copied" } else { "Copy" },
                            variant: "standard",
                            onclick: move |_| copy_response(&copy_text),
                        }
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
