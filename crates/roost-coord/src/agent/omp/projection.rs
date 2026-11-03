//! Pure ACP-to-chat projection; the ordered writer owns folding and persistence.
//! Only the final streaming assistant can accept a chunk, never earlier history.
//! Effects update OMP metadata atomically beside the transcript event batch.

use agent_client_protocol::v1::schema::{ContentBlock, SessionUpdate};
use roost_protocol::wire::agent_chat::*;

#[derive(Debug, Clone)]
pub enum OmpEffect {
    SessionReady {
        acp_session_id: String,
        config_options: serde_json::Value,
    },
    ConfigOptions(serde_json::Value),
    Title(String),
    Mode(String),
    Modes(Vec<OmpChoice>),
    RunState(AgentRunState, Option<String>),
    WorkerEpoch(Option<String>),
}
#[derive(Debug, Default)]
pub struct Projected {
    pub events: Vec<ChatEvent>,
    pub effects: Vec<OmpEffect>,
}

pub fn project(
    transcript: &Transcript,
    run_id: &str,
    seq: u64,
    event: &OmpWorkerEvent,
) -> Projected {
    let mut projected = Projected::default();
    match event {
        OmpWorkerEvent::Commands { commands } => {
            projected.events.push(ChatEvent::OmpCommands {
                commands: commands.clone(),
            });
        }
        OmpWorkerEvent::Interaction { interaction } => {
            projected.events.push(ChatEvent::OmpInteraction {
                interaction: interaction.clone(),
            });
        }
        OmpWorkerEvent::InteractionResolved { id } => {
            projected
                .events
                .push(ChatEvent::OmpInteractionResolved { id: id.clone() });
        }
        OmpWorkerEvent::Modes { modes } => {
            projected.effects.push(OmpEffect::Modes(modes.clone()));
        }
        OmpWorkerEvent::Update { update } => {
            let update = match serde_json::from_value::<SessionUpdate>(update.clone()) {
                Ok(update) => update,
                Err(error) => {
                    tracing::debug!(%error, "OMP update ignored");
                    return projected;
                }
            };
            match update {
                SessionUpdate::AgentMessageChunk(chunk) => {
                    if let ContentBlock::Text(content) = chunk.content {
                        chunk_event(
                            transcript,
                            run_id,
                            seq,
                            content.text,
                            false,
                            &mut projected.events,
                        );
                    }
                }
                SessionUpdate::AgentThoughtChunk(chunk) => {
                    if let ContentBlock::Text(content) = chunk.content {
                        chunk_event(
                            transcript,
                            run_id,
                            seq,
                            content.text,
                            true,
                            &mut projected.events,
                        );
                    }
                }
                SessionUpdate::ToolCall(call) => {
                    close_assistant(transcript, &mut projected.events);
                    append_tool_and_todo(
                        &mut projected.events,
                        super::projection_tools::new_tool(call),
                    );
                }
                SessionUpdate::ToolCallUpdate(update) => {
                    if let Some(item) = super::projection_tools::update_tool(transcript, update) {
                        append_tool_and_todo(&mut projected.events, item);
                    }
                }
                SessionUpdate::Plan(plan) => {
                    if let Some(item) = super::projection_todo::from_plan(transcript, plan) {
                        projected.events.push(ChatEvent::Item { item });
                    }
                }
                SessionUpdate::UsageUpdate(update) => {
                    let mut usage = transcript.usage.clone();
                    usage.context_tokens = update.used;
                    if let Some(cost) = update.cost.filter(|cost| cost.currency == "USD") {
                        usage.cost_usd = cost.amount;
                    }
                    projected.events.push(ChatEvent::Usage { usage });
                }
                SessionUpdate::ConfigOptionUpdate(update) => {
                    if let Ok(options) = serde_json::to_value(update.config_options) {
                        projected
                            .events
                            .push(agent_event(&options, None, transcript.mode.clone()));
                        projected.effects.push(OmpEffect::ConfigOptions(options));
                    }
                }
                SessionUpdate::CurrentModeUpdate(update) => {
                    projected.events.push(ChatEvent::Agent {
                        model: transcript.model.clone(),
                        thinking_level: transcript.thinking_level.clone(),
                        mode: Some(update.current_mode_id.to_string()),
                        omp_session: None,
                    });
                    projected
                        .effects
                        .push(OmpEffect::Mode(update.current_mode_id.to_string()));
                }
                SessionUpdate::SessionInfoUpdate(update) => {
                    if let Some(title) = update.title.take().filter(|title| !title.is_empty()) {
                        projected.effects.push(OmpEffect::Title(title));
                    }
                }
                _ => {}
            }
        }
        OmpWorkerEvent::SessionReady {
            acp_session_id,
            config_options,
            mode,
        } => {
            projected.events.push(agent_event(
                config_options,
                Some(OmpSessionState::Ready),
                mode.clone().or_else(|| transcript.mode.clone()),
            ));
            projected.effects.push(OmpEffect::SessionReady {
                acp_session_id: acp_session_id.clone(),
                config_options: config_options.clone(),
            });
            if let Some(mode) = mode {
                projected.effects.push(OmpEffect::Mode(mode.clone()));
            }
        }
        OmpWorkerEvent::SessionFailed { error } => {
            projected.events.push(ChatEvent::Agent {
                model: transcript.model.clone(),
                thinking_level: transcript.thinking_level.clone(),
                mode: transcript.mode.clone(),
                omp_session: Some(OmpSessionState::Failed {
                    error: error.clone(),
                }),
            });
        }
        OmpWorkerEvent::TurnStarted { .. } if transcript.run_state != AgentRunState::Running => {
            set_state(&mut projected, AgentRunState::Running, None)
        }
        OmpWorkerEvent::TurnEnded {
            stop_reason,
            error,
            outstanding,
            ..
        } => {
            close_open_items(transcript, &mut projected.events);
            match stop_reason.as_str() {
                "max_tokens" | "max_turn_requests" | "refusal" => projected.events.push(notice(
                    run_id,
                    seq,
                    "warning",
                    "OMP stopped early",
                    stop_reason,
                )),
                "cancelled" => projected
                    .events
                    .push(notice(run_id, seq, "info", "Stopped", "")),
                _ => {}
            }
            if *outstanding == 0 {
                set_state(
                    &mut projected,
                    if error.is_some() {
                        AgentRunState::Failed
                    } else {
                        AgentRunState::Idle
                    },
                    error.clone(),
                );
            }
        }
        OmpWorkerEvent::Notice { level, title, body } => projected
            .events
            .push(notice(run_id, seq, level, title, body)),
        OmpWorkerEvent::ProcessExited { error } => {
            close_open_items(transcript, &mut projected.events);
            for interaction in &transcript.omp_interactions {
                projected.events.push(ChatEvent::OmpInteractionResolved {
                    id: interaction.id.clone(),
                });
            }
            if let Some(error) = error {
                projected
                    .events
                    .push(notice(run_id, seq, "error", "OMP stopped", error));
            }
            if transcript.run_state == AgentRunState::Running {
                set_state(
                    &mut projected,
                    AgentRunState::Failed,
                    Some("OMP exited during a turn".into()),
                );
            }
        }
        _ => {}
    }
    projected
}

pub(super) fn close_open_items(transcript: &Transcript, events: &mut Vec<ChatEvent>) {
    for item in &transcript.items {
        if matches!(
            item,
            TranscriptItem::Tool { running: true, .. }
                | TranscriptItem::Assistant {
                    streaming: true,
                    ..
                }
        ) {
            let mut item = item.clone();
            match &mut item {
                TranscriptItem::Tool { running, .. } => *running = false,
                TranscriptItem::Assistant { streaming, .. } => *streaming = false,
                _ => {}
            }
            events.push(ChatEvent::Item { item });
        }
    }
}

pub(super) fn notice(run_id: &str, seq: u64, level: &str, title: &str, body: &str) -> ChatEvent {
    ChatEvent::Item {
        item: TranscriptItem::Notice {
            id: format!("n-{run_id}-{seq}"),
            level: level.into(),
            title: title.into(),
            body: body.into(),
        },
    }
}

fn set_state(projected: &mut Projected, state: AgentRunState, error: Option<String>) {
    projected.events.push(ChatEvent::RunState {
        run_state: state,
        error: error.clone(),
    });
    projected.effects.push(OmpEffect::RunState(state, error));
}

fn agent_event(
    options: &serde_json::Value,
    omp_session: Option<OmpSessionState>,
    mode_override: Option<String>,
) -> ChatEvent {
    let (model, thinking_level, mode) = super::catalog::current_values(options);
    ChatEvent::Agent {
        model: model.as_deref().and_then(split_omp_model),
        thinking_level,
        mode: mode_override.or(mode),
        omp_session,
    }
}

fn close_assistant(transcript: &Transcript, events: &mut Vec<ChatEvent>) {
    if let Some(TranscriptItem::Assistant {
        streaming: true, ..
    }) = transcript.items.last()
    {
        let mut item = transcript.items.last().cloned();
        if let Some(TranscriptItem::Assistant { streaming, .. }) = item.as_mut() {
            *streaming = false;
        }
        if let Some(item) = item {
            events.push(ChatEvent::Item { item });
        }
    }
}

fn chunk_event(
    transcript: &Transcript,
    run_id: &str,
    seq: u64,
    text: String,
    thinking: bool,
    events: &mut Vec<ChatEvent>,
) {
    let (item_id, block) = if let Some(TranscriptItem::Assistant {
        id,
        blocks,
        streaming: true,
        ..
    }) = transcript.items.last()
    {
        let same_kind = matches!(
            (blocks.last(), thinking),
            (Some(TranscriptBlock::Text { .. }), false)
                | (Some(TranscriptBlock::Thinking { .. }), true)
        );
        (
            id.clone(),
            if same_kind {
                blocks.len() - 1
            } else {
                blocks.len()
            },
        )
    } else {
        let id = format!("a-{run_id}-{seq}");
        events.push(ChatEvent::Item {
            item: TranscriptItem::Assistant {
                id: id.clone(),
                blocks: vec![],
                streaming: true,
                error: None,
            },
        });
        (id, 0)
    };
    events.push(if thinking {
        ChatEvent::ThinkingDelta {
            item_id,
            block,
            delta: text,
        }
    } else {
        ChatEvent::TextDelta {
            item_id,
            block,
            delta: text,
        }
    });
}

fn append_tool_and_todo(events: &mut Vec<ChatEvent>, item: TranscriptItem) {
    let todo = super::projection_todo::from_tool(&item);
    events.push(ChatEvent::Item { item });
    if let Some(item) = todo {
        events.push(ChatEvent::Item { item });
    }
}

#[cfg(test)]
mod tests;
