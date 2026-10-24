//! A built-in agent conversation in the sidebar, linked to its chat surface.
//! Conversation metadata comes from the client store; folder and Agents
//! projections both use this row.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::components::ui::IconType;
use crate::components::ui::list::ListRow;
use crate::components::ui::status::StatusDot;
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
    // Under a folder the row sits in the tree's indent and one dense line tall,
    // so the tree reads as one list. The `group` root lets the texts recolor
    // with the selection, as the session rows do.
    let row_class = if compact {
        "group ps-9 gap-2 min-h-7"
    } else {
        "group"
    };
    rsx! {
        ListRow {
            leading_icon: Some(IconType::Bot),
            headline: rsx! { span { class: "min-w-0 truncate text-sm text-foreground group-data-[selected=true]:text-accent-foreground", "{crate::components::agent_chat::display_conversation_title(&conversation.title)}" } },
            // Under a folder the row is one line, like the session rows: the
            // model is the chat's business, not the tree's.
            support: (!compact).then(|| rsx! { span { class: "min-w-0 truncate text-xs text-muted-foreground group-data-[selected=true]:text-accent-foreground", "{model_name}" } }),
            trailing: Some(rsx! {
                StatusDot {
                    status: status.to_owned(),
                    title: Some(status_title.to_owned()),
                }
            }),
            href: Some(href.clone()),
            on_navigate: Some(navigate_to_agent),
            selected,
            aria_current: selected.then_some("page".to_owned()),
            dense: compact,
            test_id: Some(format!("sidebar-agent-conversation-{conversation_id}")),
            class: Some(row_class.to_owned()),
        }
    }
}
