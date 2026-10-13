//! The built-in agent's chat surface, mounted by the terminal deck's agent slot
//! for an `agent:<id>` tab (route `/a/:conversationId`), and the one call every
//! "Start agent here" affordance makes. The transcript and the conversation
//! rows are the store's (`roost_client_core::client::agent_chat`); this module
//! renders them and sends the UI-direct `AgentChat*` calls.

mod advisory_card;
mod composer;
mod composer_commands;
mod composer_helpers;
mod composer_status;
mod goal_controls;
mod goal_dock;
mod header;
mod interaction_cards;
mod launch;
mod loop_controls;
mod markdown;
mod message_actions;
mod model_controls;
pub mod model_picker;
mod notice_card;
mod plan_card;
mod request_ids;
mod slash_menu;
mod surface;
#[cfg(target_arch = "wasm32")]
mod surface_fetch;
mod surface_state;
mod thinking_block;
mod tool_card;
mod toolbar_menu;
mod transcript;
mod welcome;

pub use launch::launch_agent;
pub use surface::AgentChatSurface;
pub use toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
