//! A built-in agent conversation in the sidebar, linked to its chat surface.
//! Conversation metadata comes from the client store; folder and Agents
//! projections both use this row.

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;
use libero::{
    components::{Box, Flex, Indicator, Paper, Pictogram, Text},
    sx::sx,
};
use roost_client_core::ClientEvent;
use roost_client_core::client::agents::status_policy::AgentDotStatus;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::pump::use_store;
use crate::router_state::{is_in_app_navigation_click, use_navigate};

/// A sidebar destination for one built-in agent conversation.
#[component]
pub fn AgentConversationRow(
    conversation_id: String,
    selected: bool,
    #[props(default)] compact: bool,
) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
    let title_sx = use_hook(|| {
        sx().min_width("0")
            .flex("1")
            .flex_direction("column")
            .gap("xs")
    });
    let row_sx = use_hook(|| {
        sx().display("flex")
            .align_items("center")
            .gap("sm")
            .padding_inline("md")
            .padding_block("sm")
            .selector("&:hover", sx().background("primary.1"))
            .selector("&[data-selected=true]", sx().background("primary.2"))
    });
    let link_sx = use_hook(|| {
        sx().display("block")
            .color("inherit")
            .text_decoration("none")
            .selector(
                "&:focus-visible",
                sx().outline("2px solid var(--workbench-focus-ring)")
                    .outline_offset("2px"),
            )
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
    let (dot_status, status_title) = match conversation.run_state {
        roost_protocol::wire::agent_chat::AgentRunState::Running => {
            (AgentDotStatus::Info, "Running")
        }
        roost_protocol::wire::agent_chat::AgentRunState::Failed => (AgentDotStatus::Warn, "Failed"),
        roost_protocol::wire::agent_chat::AgentRunState::Idle => (AgentDotStatus::Idle, "Idle"),
    };
    let status = crate::components::agents::agent_status_indicator::agent_dot_color(dot_status);
    let row_id = format!("sidebar-agent-conversation-{conversation_id}");
    rsx! {
        Box { component: "a",
            sx: link_sx,
            href: href.clone(),
            "data-testid": row_id,
            "aria-current": if selected { Some("page") } else { None },
            "data-selected": selected,
            onclick: move |event: MouseEvent| {
                if !is_in_app_navigation_click(
                    event.trigger_button() == Some(MouseButton::Primary),
                    event.modifiers(),
                ) {
                    return;
                }
                event.prevent_default();
                pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));
                navigate.call(href.clone());
            },
            Paper { sx: row_sx, "data-selected": selected,
                Flex { direction: "row", wrap: false, gap: "sm",
                    Pictogram { icon: pictogram_icons_lucide::bot::outlined, width: "1rem", height: "1rem" }
                    Flex { sx: title_sx, direction: "column", gap: "xs",
                        Text { size: "sm", "{crate::components::agent_chat::display_conversation_title(&conversation.title)}" }
                        if !compact { Text { size: "xs", color: "muted", "{model_name}" } }
                    }
                    Indicator { color: status, size: "xs", title: status_title }
                }
            }
        }
    }
}
