//! OMP rows projected into the shared conversation list shape.
//! Settings changes and committed worker frames rebuild this same summary.
//! Native model roles and settings never participate in OMP resolution.

use super::store::OmpConversation;
use roost_protocol::wire::agent_chat::*;

pub fn summary(
    conversation: &OmpConversation,
    scopes: &[OmpScopedSettings],
) -> ConversationSummary {
    let effective = resolve_omp_settings(
        scopes,
        &conversation.worker_fp,
        &conversation.cwd,
        &conversation.override_values,
    );
    ConversationSummary {
        backend: AgentBackend::Omp,
        id: conversation.id.clone(),
        title: conversation.title.clone(),
        worker_fp: conversation.worker_fp.clone(),
        worker_label: conversation.worker_label.clone(),
        cwd: conversation.cwd.clone(),
        model: conversation
            .override_values
            .model
            .as_deref()
            .and_then(split_omp_model),
        effective_model: effective
            .model
            .value
            .as_deref()
            .or(conversation.current_model.as_deref())
            .and_then(split_omp_model),
        thinking_level: conversation.override_values.thinking_level.clone(),
        mode: "normal".into(),
        parent_id: conversation.parent_id.clone(),
        agent: None,
        advisor: false,
        run_state: conversation.run_state,
        error: conversation.error.clone(),
        created_ms: conversation.created_ms,
        updated_ms: conversation.updated_ms,
    }
}
