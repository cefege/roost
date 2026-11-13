//! The built-in agent's chat surface, mounted by the terminal deck's agent slot
//! for an `agent:<id>` tab (route `/a/:conversationId`), and the one call every
//! "Start agent here" affordance makes. The transcript and the conversation
//! rows are the store's (`roost_client_core::client::agent_chat`); this module
//! renders them and sends the UI-direct `AgentChat*` calls.

pub mod agent_alert;
mod bash_box;
mod diff_model;
mod diff_view;
mod header;
pub(crate) mod last_conversation;
mod launch;
mod markdown;
mod markdown_figures;
pub(crate) mod omp_controls;
mod omp_form_field;
mod omp_interaction_card;
mod omp_ui_mount;
mod omp_ui_mount_transport;
mod output_lines;
mod output_window;
mod request_ids;
pub mod semantic_ui;
mod surface;
mod surface_dom;
#[cfg(target_arch = "wasm32")]
mod surface_fetch;
mod surface_state;
mod syntax_output;
mod task_rows;
pub(crate) mod timeline_rail;
mod tool_card;
mod tool_view;
mod toolbar_menu;
pub(crate) mod working_row;
mod write_view;

pub(crate) use header::AgentChatHeader;
pub use launch::launch_agent;
pub use surface::AgentChatSurface;
pub use toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};

pub(crate) fn display_conversation_title(title: &str) -> &str {
    if title.trim().is_empty() {
        "New conversation"
    } else {
        title
    }
}
