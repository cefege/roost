//! Connection-generation events and lifecycle state for one OMP conversation.
//! The actor retains request identity across child restarts.

use super::{
    actor::OmpCommand, interaction_pending::PendingInteractions, launcher::CatalogReadiness,
};
use agent_client_protocol::{
    Error,
    v1::schema::{PromptResponse, SessionNotification},
};
use roost_proto::{DOmpOpen, DOmpPrompt};
use roost_protocol::wire::agent_chat::OmpInteraction;
use std::collections::VecDeque;

#[expect(
    clippy::large_enum_variant,
    reason = "Streaming notifications dominate this channel; boxing adds an allocation to every ACP chunk."
)]
pub(super) enum AcpEvent {
    Notification(SessionNotification),
    Ui(serde_json::Value),
    UiState(serde_json::Value),
    /// A native action asked for a browser-owned effect; carries no durable data.
    HostService(serde_json::Value),
    /// The native action withdrew its own request (its own timeout).
    HostServiceCancel(serde_json::Value),
    DashboardResource(serde_json::Value),
    UiCompleted {
        method: &'static str,
        transport_id: String,
        request_id: String,
        result: Result<serde_json::Value, Error>,
    },
    Completed {
        request_id: String,
        result: Result<PromptResponse, Error>,
    },
    Interaction {
        interaction: OmpInteraction,
        respond: Box<dyn FnOnce(roost_proto::DOmpRespond) -> Result<(), Error> + Send>,
    },
}

pub(super) struct ConnectionProgress {
    pub prompt_first: bool,
    pub has_started_turn: bool,
    pub catalog: Option<CatalogReadiness>,
}

pub(super) struct ConnectionStart<'a> {
    pub prompt: DOmpPrompt,
    pub initial_ui: Option<roost_proto::DOmpUi>,
    pub ui: &'a mut super::ui::UiState,
    pub progress: &'a mut ConnectionProgress,
    pub pending: &'a mut PendingInteractions,
    pub deferred: &'a mut VecDeque<OmpCommand>,
}

pub(super) enum ConnectionExit {
    Closed,
    Idle,
    Refresh(DOmpOpen),
}
