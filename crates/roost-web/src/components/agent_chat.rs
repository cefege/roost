//! The built-in agent's chat surface, mounted by the terminal deck's agent slot
//! for an `agent:<id>` tab (route `/a/:conversationId`), and the one call every
//! "Start agent here" affordance makes. The transcript and the conversation
//! rows are the store's (`roost_client_core::client::agent_chat`); this module
//! renders them and sends the UI-direct `AgentChat*` calls.

#[cfg(feature = "archived-native-ui")]
mod advisory_card;
pub mod agent_alert;
#[cfg(feature = "archived-native-ui")]
mod background_job_box;
mod bash_box;
#[cfg(feature = "archived-native-ui")]
mod composer;
#[cfg(feature = "archived-native-ui")]
mod composer_attachments;
#[cfg(feature = "archived-native-ui")]
mod composer_commands;
#[cfg(feature = "archived-native-ui")]
mod composer_footer;
#[cfg(feature = "archived-native-ui")]
mod composer_helpers;
#[cfg(feature = "archived-native-ui")]
mod composer_status;
#[cfg(feature = "archived-native-ui")]
mod composer_submit;
mod diff_model;
mod diff_view;
#[cfg(feature = "archived-native-ui")]
mod draft;
#[cfg(feature = "archived-native-ui")]
mod goal_controls;
mod header;
#[cfg(feature = "archived-native-ui")]
mod inspector;
#[cfg(feature = "archived-native-ui")]
mod inspector_panels;
#[cfg(feature = "archived-native-ui")]
mod interaction_cards;
pub(crate) mod last_conversation;
mod launch;
#[cfg(feature = "archived-native-ui")]
mod loop_controls;
mod markdown;
#[cfg(feature = "archived-native-ui")]
mod message_actions;
#[cfg(feature = "archived-native-ui")]
mod model_controls;
#[cfg(feature = "archived-native-ui")]
pub mod model_hub;
#[cfg(feature = "archived-native-ui")]
pub mod model_picker;
#[cfg(feature = "archived-native-ui")]
mod model_reference_actions;
#[cfg(feature = "archived-native-ui")]
mod model_reference_menu;
#[cfg(feature = "archived-native-ui")]
mod notice_card;
pub(crate) mod omp_controls;
mod omp_form_field;
mod omp_interaction_card;
#[cfg(feature = "archived-native-ui")]
mod omp_setting_menu;
mod omp_ui_mount;
mod omp_ui_mount_transport;
mod output_lines;
mod output_window;
#[cfg(feature = "archived-native-ui")]
mod peer_message_card;
#[cfg(feature = "archived-native-ui")]
mod plan_card;
#[cfg(feature = "archived-native-ui")]
mod plan_refinement;
#[cfg(feature = "archived-native-ui")]
mod provider_usage;
#[cfg(feature = "archived-native-ui")]
mod queued_band;
mod request_ids;
#[cfg(feature = "archived-native-ui")]
mod role_presets;
#[cfg(feature = "archived-native-ui")]
mod role_switch;
#[cfg(feature = "archived-native-ui")]
mod role_switcher;
#[cfg(feature = "archived-native-ui")]
mod role_thinking;
pub mod semantic_ui;
#[cfg(feature = "archived-native-ui")]
mod slash_menu;
#[cfg(feature = "archived-native-ui")]
mod slash_omp;
#[cfg(feature = "archived-native-ui")]
mod streaming_reveal;
#[cfg(feature = "archived-native-ui")]
mod submission;
mod surface;
mod surface_dom;
#[cfg(target_arch = "wasm32")]
mod surface_fetch;
mod surface_state;
mod syntax_output;
mod task_rows;
#[cfg(feature = "archived-native-ui")]
mod thinking_block;
pub(crate) mod timeline_rail;
mod tool_card;
mod tool_view;
mod toolbar_menu;
#[cfg(feature = "archived-native-ui")]
mod transcript;
#[cfg(feature = "archived-native-ui")]
mod welcome;
pub(crate) mod working_row;
#[cfg(feature = "archived-native-ui")]
mod workspace;
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
