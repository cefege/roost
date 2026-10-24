//! The built-in agent's chat surface, mounted by the terminal deck's agent slot
//! for an `agent:<id>` tab (route `/a/:conversationId`), and the one call every
//! "Start agent here" affordance makes. The transcript and the conversation
//! rows are the store's (`roost_client_core::client::agent_chat`); this module
//! renders them and sends the UI-direct `AgentChat*` calls.

mod advisory_card;
pub mod agent_alert;
mod ask_card;
mod background_job_box;
mod bash_box;
mod bash_footer;
mod btw_card;
mod composer;
mod composer_commands;
mod composer_footer;
mod composer_helpers;
mod composer_options;
mod composer_status;
mod context_usage;
mod diff_model;
mod diff_view;
mod draft;
mod goal_controls;
mod goal_dock;
mod header;
mod inspector;
mod inspector_panels;
mod launch;
mod loop_controls;
mod message_actions;
mod model_controls;
pub mod model_hub;
pub mod model_picker;
mod notice_card;
mod output_lines;
mod output_window;
mod peer_message_card;
mod plan_card;
mod queued_band;
mod request_ids;
mod role_presets;
mod role_switch;
mod role_switcher;
mod role_thinking;
mod slash_menu;
mod streaming_reveal;
mod submission;
mod surface;
mod surface_dom;
#[cfg(target_arch = "wasm32")]
mod surface_fetch;
mod surface_state;
mod task_rows;
mod thinking_block;
mod todo_card;
mod tool_card;
mod tool_view;
mod toolbar_menu;
mod transcript;
mod welcome;
pub(crate) mod working_row;
mod workspace;
mod write_view;

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
