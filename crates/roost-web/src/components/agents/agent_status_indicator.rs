//! One session's agent status chip: the level's `StatusDot` and, unless
//! compact, its label, hidden while the session has no status or an unknown
//! one. Ports `apps/web/src/components/agents/AgentStatusIndicator.tsx`; the
//! sidebar's session and agent rows render it. The level and tooltip come from
//! `roost_client_core::client::agents::status_policy`.

use dioxus::prelude::*;
use roost_client_core::client::agents::status_policy::{
    AgentDotStatus, AgentStatusLevel, agent_status_level_token, agent_status_presentation,
    agent_status_tooltip, derive_agent_status_level,
};
use roost_protocol::wire::SessionId;

use crate::components::md::StatusDot;
use crate::pump::use_store;

/// The `StatusDot` status name a level's dot paints with.
pub const fn agent_dot_status_name(dot: AgentDotStatus) -> &'static str {
    match dot {
        AgentDotStatus::Warn => "warn",
        AgentDotStatus::Ok => "ok",
        AgentDotStatus::Info => "info",
        AgentDotStatus::Idle => "idle",
    }
}

/// The chip for `session_id`.
#[component]
pub fn AgentStatusIndicator(
    session_id: String,
    #[props(default)] compact: bool,
    /// Surfaces with their own hover card suppress the native tooltip.
    #[props(default)]
    suppress_tooltip: bool,
    class: Option<String>,
) -> Element {
    let pump = use_store();
    let core = pump.core();
    let core = core.borrow();
    let store = core.store();
    let Some(status) = SessionId::try_from(session_id.clone())
        .ok()
        .and_then(|id| store.agent_status.status(&id))
    else {
        return rsx! {};
    };
    let acknowledged = Some(store.agent_seen.acknowledged_revision(status));
    let level = derive_agent_status_level(Some(status), acknowledged);
    if level == AgentStatusLevel::Unknown {
        return rsx! {};
    }
    let presentation = agent_status_presentation(level);
    let class_name = format!(
        "agent-status {} {}",
        if compact { "agent-status--compact" } else { "" },
        class.as_deref().unwrap_or("")
    );
    let title = (!suppress_tooltip).then(|| agent_status_tooltip(status, acknowledged));
    rsx! {
        span {
            class: class_name.trim().to_owned(),
            "data-testid": "agent-status-{session_id}",
            "data-level": agent_status_level_token(level),
            title,
            role: "img",
            "aria-label": presentation.label,
            StatusDot { status: agent_dot_status_name(presentation.dot_status).to_owned() }
            if !compact {
                span { class: "agent-status__label", {presentation.label} }
            }
        }
    }
}
