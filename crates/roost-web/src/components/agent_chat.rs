//! The built-in agent's chat surface at `/a/:conversationId` and the one call
//! every "Start agent here" affordance makes. The transcript and the
//! conversation rows are the store's (`roost_client_core::client::agent_chat`);
//! this module renders them and sends the UI-direct `AgentChat*` calls.
//! Mounted by `app::RouteContent`; `launch` is called by the folder picker and
//! the sidebar's folder menu.

mod composer;
mod header;
mod launch;
mod markdown;
mod surface;
mod tool_card;
mod transcript;

pub use launch::launch_agent;
pub use surface::AgentChatSurface;
