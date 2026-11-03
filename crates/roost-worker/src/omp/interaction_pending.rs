//! Owns ACP responders until an explicit decision or safe cancellation.
//! Startup and connected-session loops share this lifecycle.

use super::{actor_event, actor_types::AcpEvent, emitter::Emitter};
use agent_client_protocol::Error;
use roost_proto::DOmpRespond;
use roost_protocol::wire::agent_chat::{
    OmpInteraction, OmpWorkerEvent, validate_omp_interaction_response,
};
use std::collections::HashMap;

pub(super) type PendingInteractions = HashMap<String, PendingInteraction>;

pub(super) struct PendingInteraction {
    pub interaction: OmpInteraction,
    respond: Box<dyn FnOnce(DOmpRespond) -> Result<(), Error> + Send>,
}

pub(super) fn hold_interaction(
    interaction: OmpInteraction,
    respond: Box<dyn FnOnce(DOmpRespond) -> Result<(), Error> + Send>,
    pending: &mut PendingInteractions,
    emitter: &mut Emitter,
) {
    pending.insert(
        interaction.id.clone(),
        PendingInteraction {
            interaction: interaction.clone(),
            respond,
        },
    );
    emitter.emit(OmpWorkerEvent::Interaction { interaction });
    emitter.flush();
}

pub(super) fn respond_interaction(
    mut response: DOmpRespond,
    pending: &mut PendingInteractions,
    emitter: &mut Emitter,
) {
    if response.conversation_id != emitter.conversation_id {
        emitter.notice(
            "warning",
            "Response rejected",
            "This interaction belongs to another OMP conversation.",
        );
        return;
    }
    if let Some(interaction) = pending.remove(&response.request_id) {
        if let Ok(content) = validate_omp_interaction_response(
            &interaction.interaction,
            &response.request_id,
            &response.action,
            &response.content_json,
        ) {
            if matches!(response.action.as_str(), "accept" | "permission") {
                response.content_json = content.to_string();
            }
            if let Err(error) = (interaction.respond)(response) {
                emitter.notice("error", "Response failed", &error.to_string());
            }
            emitter.emit(OmpWorkerEvent::InteractionResolved {
                id: interaction.interaction.id,
            });
        } else {
            emitter.notice(
                "warning",
                "Response rejected",
                "The response does not match the pending ACP request.",
            );
            pending.insert(interaction.interaction.id.clone(), interaction);
        }
    } else {
        emitter.notice(
            "warning",
            "Request expired",
            "This interaction is no longer pending.",
        );
    }
}

pub(super) fn clear_interactions(pending: &mut PendingInteractions, emitter: &mut Emitter) {
    for (id, pending) in pending.drain() {
        let _ = (pending.respond)(DOmpRespond {
            conversation_id: emitter.conversation_id.clone(),
            request_id: id.clone(),
            action: "cancel".into(),
            ..Default::default()
        });
        emitter.emit(OmpWorkerEvent::InteractionResolved { id });
    }
}

pub(super) fn consume_open_event(
    event: AcpEvent,
    pending: &mut PendingInteractions,
    emitter: &mut Emitter,
) {
    match event {
        AcpEvent::Interaction {
            interaction,
            respond,
        } => {
            hold_interaction(interaction, respond, pending, emitter);
        }
        AcpEvent::Notification(notification) => {
            if let agent_client_protocol::v1::schema::SessionUpdate::AvailableCommandsUpdate(
                update,
            ) = notification.update
            {
                actor_event::emit_commands(&update, emitter);
            }
        }
        _ => {}
    }
}

pub(super) fn drain_pending_events(
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AcpEvent>,
    pending: &mut PendingInteractions,
    emitter: &mut Emitter,
) {
    while let Ok(event) = events.try_recv() {
        consume_open_event(event, pending, emitter);
    }
}
