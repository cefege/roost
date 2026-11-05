//! Semantic UI projection arms: durable frames, diagnostics and the live
//! native transport token. Kept beside the pure projector so the ACP arm stays
//! readable and the token/effect pairing lives in one place.

use super::projection::{OmpEffect, Projected};
use roost_protocol::wire::agent_chat::{ChatEvent, OmpWorkerEvent, Transcript};

/// Project one semantic UI worker event. Returns whether it handled the event.
pub(super) fn project_event(projected: &mut Projected, event: &OmpWorkerEvent) -> bool {
    match event {
        OmpWorkerEvent::Ui { message } => {
            projected.events.push(ChatEvent::OmpUi {
                message: message.clone(),
            });
            true
        }
        OmpWorkerEvent::UiReset { transport_id } => {
            projected
                .effects
                .push(OmpEffect::UiTransport(transport_id.clone()));
            projected.events.push(ChatEvent::OmpUiReset {
                transport_id: transport_id.clone(),
            });
            true
        }
        OmpWorkerEvent::UiFailed { error } => {
            projected.events.push(ChatEvent::OmpUiError {
                error: error.clone(),
            });
            true
        }
        _ => false,
    }
}

/// Close the live native transport before an exited session is reloaded.
pub(super) fn close(transcript: &Transcript, projected: &mut Projected) {
    let mut open = transcript.omp_ui_transport_id.is_some()
        || transcript.omp_ui.is_some()
        || transcript.omp_ui_error.is_some();
    for event in &projected.events {
        match event {
            ChatEvent::OmpUiReset { transport_id } => open = transport_id.is_some(),
            ChatEvent::OmpUi { .. } | ChatEvent::OmpUiError { .. } => open = true,
            _ => {}
        }
    }
    if open {
        projected.effects.push(OmpEffect::UiTransport(None));
        projected
            .events
            .push(ChatEvent::OmpUiReset { transport_id: None });
    }
}
