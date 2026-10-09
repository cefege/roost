//! Conversion between agent conversation summaries and their sync protobuf.

use roost_proto::AgentConversation as PbAgentConversation;

use super::model::{AgentRunState, ConversationSummary, ModelRef};

pub fn conversation_to_proto(summary: &ConversationSummary) -> PbAgentConversation {
    PbAgentConversation {
        id: summary.id.clone(),
        title: summary.title.clone(),
        worker_fp: summary.worker_fp.clone(),
        worker_label: summary.worker_label.clone(),
        cwd: summary.cwd.clone(),
        model_provider: summary
            .model
            .as_ref()
            .map_or_else(String::new, |model| model.provider.clone()),
        model_id: summary
            .model
            .as_ref()
            .map_or_else(String::new, |model| model.model_id.clone()),
        thinking_level: summary.thinking_level.clone().unwrap_or_default(),
        run_state: summary.run_state.as_str().to_owned(),
        error: summary.error.clone().unwrap_or_default(),
        created_ms: summary.created_ms,
        updated_ms: summary.updated_ms,
        ..Default::default()
    }
}

pub fn conversation_from_proto(message: &PbAgentConversation) -> ConversationSummary {
    ConversationSummary {
        id: message.id.clone(),
        title: message.title.clone(),
        worker_fp: message.worker_fp.clone(),
        worker_label: message.worker_label.clone(),
        cwd: message.cwd.clone(),
        model: if message.model_provider.is_empty() && message.model_id.is_empty() {
            None
        } else {
            Some(ModelRef {
                provider: message.model_provider.clone(),
                model_id: message.model_id.clone(),
            })
        },
        thinking_level: (!message.thinking_level.is_empty())
            .then(|| message.thinking_level.clone()),
        run_state: AgentRunState::from_wire(&message.run_state),
        error: (!message.error.is_empty()).then(|| message.error.clone()),
        created_ms: message.created_ms,
        updated_ms: message.updated_ms,
    }
}
