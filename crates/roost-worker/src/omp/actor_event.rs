//! Projects ACP session events into the worker's canonical OMP event stream.
//! Conversation actor delegates notifications and prompt completion here.

use super::{
    actor_types::AcpEvent,
    emitter::Emitter,
    interaction_pending::{self, PendingInteractions},
    session,
};
use agent_client_protocol::v1::schema::{
    AvailableCommandInput, AvailableCommandsUpdate, SessionUpdate,
};
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
pub(super) fn consume_event(
    event: AcpEvent,
    session: &mut session::Session,
    emitter: &mut Emitter,
    outstanding: &mut u32,
) {
    match event {
        AcpEvent::Notification(notification) if notification.session_id == session.id => {
            session::update_config(session, &notification.update);
            if let SessionUpdate::AvailableCommandsUpdate(update) = &notification.update {
                emit_commands(update, emitter);
            }
            if let Ok(update) = serde_json::to_value(notification.update) {
                emitter.emit(OmpWorkerEvent::Update { update });
            }
        }
        AcpEvent::Completed { request_id, result } => {
            *outstanding = outstanding.saturating_sub(1);
            let (stop_reason, error) = match result {
                Ok(response) => (
                    serde_json::to_value(response.stop_reason)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "end_turn".into()),
                    None,
                ),
                Err(error) => ("error".into(), Some(error.to_string())),
            };
            tracing::info!(conversation_id=%emitter.conversation_id,%request_id,%stop_reason,"OMP turn ended");
            emitter.emit(OmpWorkerEvent::TurnEnded {
                request_id,
                stop_reason,
                error,
                outstanding: *outstanding,
            });
            emitter.flush();
        }
        _ => {}
    }
}

pub(super) fn emit_commands(update: &AvailableCommandsUpdate, emitter: &mut Emitter) {
    emitter.emit(OmpWorkerEvent::Commands {
        commands: update
            .available_commands
            .iter()
            .map(|command| {
                let input_hint = command.input.as_ref().and_then(|input| match input {
                    AvailableCommandInput::Unstructured(input) => Some(input.hint.clone()),
                    _ => None,
                });
                roost_protocol::wire::agent_chat::OmpSlashCommand {
                    name: command.name.clone(),
                    description: command.description.clone(),
                    input_hint,
                }
            })
            .collect(),
    });
}

pub(super) fn drain_disconnect(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AcpEvent>,
    session: &mut session::Session,
    emitter: &mut Emitter,
    outstanding: &mut u32,
    pending: &mut PendingInteractions,
) {
    while let Ok(event) = events.try_recv() {
        match event {
            AcpEvent::Interaction {
                interaction,
                respond,
            } => {
                interaction_pending::hold_interaction(interaction, respond, pending, emitter);
            }
            event => {
                if matches!(&event, AcpEvent::Completed { .. }) && *outstanding <= 1 {
                    interaction_pending::clear_interactions(pending, emitter);
                }
                consume_event(event, session, emitter, outstanding);
            }
        }
    }
}
