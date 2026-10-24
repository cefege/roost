//! One session's agent status chip: the level's Libero indicator and, unless
//! compact, its label, hidden while the session has no status or an unknown
//! one. The level and tooltip come from
//! `roost_client_core::client::agents::status_policy`.

use dioxus::prelude::*;
use libero::components::{Flex, Indicator, Text};
use roost_client_core::client::agents::status_policy::{
    AgentDotStatus, AgentStatusLevel, agent_status_level_token, agent_status_presentation,
    agent_status_tooltip, derive_agent_status_level,
};
use roost_protocol::wire::SessionId;

use crate::pump::use_store;

/// The Libero indicator color for an agent status dot.
pub const fn agent_dot_color(dot: AgentDotStatus) -> &'static str {
    match dot {
        AgentDotStatus::Warn => "warning",
        AgentDotStatus::Ok => "success",
        AgentDotStatus::Info => "info",
        AgentDotStatus::Idle => "muted",
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
    let title = (!suppress_tooltip).then(|| agent_status_tooltip(status, acknowledged));
    let text_color = match level {
        AgentStatusLevel::Blocked => "warning",
        AgentStatusLevel::Done => "success",
        AgentStatusLevel::Working => "info",
        _ => "muted",
    };
    rsx! {
        Flex {
            direction: "row",
            align: "center",
            gap: "xs",
            wrap: false,
            "data-testid": "agent-status-{session_id}",
            "data-level": agent_status_level_token(level),
            title,
            role: "img",
            "aria-label": presentation.label,
            Indicator {
                color: agent_dot_color(presentation.dot_status),
                size: "xs",
                aria_hidden: "true",
            }
            if !compact {
                Text { size: "sm", color: text_color, "{presentation.label}" }
            }
        }
    }
}
