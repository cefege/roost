//! A built-in agent conversation in the sidebar, linked to its chat surface.
//! Conversation metadata comes from the client store; folder and Agents
//! projections both use this row.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::components::md::{ListRow, StatusDot};
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// A sidebar destination for one built-in agent conversation.
#[component]
pub fn AgentConversationRow(
    conversation_id: String,
    selected: bool,
    #[props(default)] compact: bool,
) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let navigate_to_agent = EventHandler::new({
        let pump = pump.clone();
        move |href: String| {
            pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));
            navigate.call(href);
        }
    });
    let conversation = {
        let core = pump.core();
        let core = core.borrow();
        core.store()
            .agent_chat
            .conversations
            .get(&conversation_id)
            .cloned()
    };
    let Some(conversation) = conversation else {
        return rsx! {};
    };
    let href = crate::routes::agent_href(&conversation_id);
    let model_name = conversation
        .model
        .as_ref()
        .map(|model| model.model_id.as_str())
        .unwrap_or("No model");
    let (status, status_title) = match conversation.run_state {
        roost_protocol::wire::agent_chat::AgentRunState::Running => ("info", "Running"),
        roost_protocol::wire::agent_chat::AgentRunState::Failed => ("warn", "Failed"),
        roost_protocol::wire::agent_chat::AgentRunState::Idle => ("idle", "Idle"),
    };
    rsx! {
        ListRow {
            leading_icon: "smart_toy",
            headline: rsx! { span { class: "workbench-sidebar-agent-conversation__title", "{conversation.title}" } },
            // Under a folder the row is one line, like the session rows: the
            // model is the chat's business, not the tree's.
            support: (!compact).then(|| rsx! { span { class: "workbench-sidebar-agent-conversation__model", "{model_name}" } }),
            trailing: rsx! {
                StatusDot {
                    status: status.to_owned(),
                    title: status_title.to_owned(),
                }
            },
            href: href.clone(),
            on_navigate: navigate_to_agent,
            selected,
            aria_current: selected.then_some("page".to_owned()),
            dense: compact,
            test_id: "sidebar-agent-conversation-{conversation_id}",
            class: compact.then(|| "workbench-sidebar-agent-conversation".to_owned()),
        }
    }
}
