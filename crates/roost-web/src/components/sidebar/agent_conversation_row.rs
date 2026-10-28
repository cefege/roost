//! One built-in agent destination in the folder navigation tree.
//! `FolderList` owns its parent/child hierarchy and selection; this row uses
//! the stock Sidenav sub-link and preserves native modified-click navigation.

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::store::sidebar::SidebarIntent;

use crate::components::ui::list::is_in_app_navigation_click;
use crate::components::ui::{
    Icon, IconType, SidenavMenuButton, SidenavMenuButtonSize, SidenavMenuSubButton, StatusDot,
};
use crate::pump::use_store;
use crate::router_state::use_navigate;

/// A sidebar destination for one built-in agent conversation.
#[component]
pub fn AgentConversationRow(
    conversation_id: String,
    selected: bool,
    search_result: bool,
    query: String,
) -> Element {
    let pump = use_store();
    let navigate = use_navigate();
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
    let (status, status_title) = match conversation.run_state {
        roost_protocol::wire::agent_chat::AgentRunState::Running => ("running", "Running"),
        roost_protocol::wire::agent_chat::AgentRunState::Failed => ("warn", "Failed"),
        roost_protocol::wire::agent_chat::AgentRunState::Idle => ("idle", "Idle"),
    };
    let on_click = EventHandler::new({
        let pump = pump.clone();
        let href = href.clone();
        move |event: MouseEvent| {
            let primary =
                event.trigger_button() == Some(dioxus::html::input_data::MouseButton::Primary);
            if is_in_app_navigation_click(primary, event.modifiers()) {
                event.prevent_default();
                pump.dispatch(ClientEvent::Sidebar(SidebarIntent::CloseDrawer));
                navigate.call(href.clone());
            }
        }
    });
    let title = crate::components::agent_chat::display_conversation_title(&conversation.title);
    let model = conversation
        .model
        .as_ref()
        .or(conversation.effective_model.as_ref())
        .map(|model| model.model_id.clone());
    let row_content = rsx! {
        Icon { icon: IconType::Bot }
        if search_result {
            span { class: "flex min-w-0 flex-1 flex-col text-start",
                span { class: "truncate text-sm font-medium",
                    crate::components::global_search::metadata::HighlightedText {
                        text: title.to_owned(),
                        query,
                    }
                }
                if let Some(model) = model {
                    span { class: "truncate text-xs text-muted-foreground", "{model}" }
                }
            }
        } else {
            span { class: "min-w-0 flex-1 truncate", "{title}" }
        }
        StatusDot { status: status.to_owned(), title: status_title.to_owned() }
    };
    if search_result {
        rsx! {
            SidenavMenuButton {
                href: Some(href),
                size: SidenavMenuButtonSize::Lg,
                aria_current: if selected { "page" } else { "false" },
                "data-testid": "sidebar-agent-conversation-{conversation_id}",
                onclick: Some(on_click),
                {row_content}
            }
        }
    } else {
        rsx! {
            SidenavMenuSubButton {
                href,
                aria_current: if selected { "page" } else { "false" },
                "data-testid": "sidebar-agent-conversation-{conversation_id}",
                onclick: Some(on_click),
                {row_content}
            }
        }
    }
}
