//! The built-in agent's chat surface, mounted by the terminal deck's agent slot
//! for an `agent:<id>` tab (route `/a/:conversationId`), and the one call every
//! "Start agent here" affordance makes. The transcript and the conversation
//! rows are the store's (`roost_client_core::client::agent_chat`); this module
//! renders them and sends the UI-direct `AgentChat*` calls.

mod advisory_card;
mod composer;
mod header;
mod launch;
mod markdown;
mod model_controls;
mod notice_card;
mod plan_card;
mod slash_menu;
mod surface;
mod tool_card;
mod toolbar_menu;
mod transcript;
mod welcome;

pub use launch::launch_agent;
pub use surface::AgentChatSurface;
pub use toolbar_menu::{ToolbarMenu, ToolbarMenuItem, ToolbarTrigger};
