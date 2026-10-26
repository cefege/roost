//! Remembers the agent conversation the operator last had focused, so a boot
//! on any laptop can land there. Written through the synced store, so the
//! memory follows the account rather than one browser.

use dioxus::prelude::*;

use crate::pump::use_pump;

/// Synced client-state key holding the last focused agent conversation id.
/// The boot landing reads the same key.
pub(crate) const LAST_CONVERSATION_KEY: &str = "roost.lastAgentConversation";

/// Records `conversation_id` whenever its surface becomes the focused one.
/// Only the focused tab writes, so a background mount cannot overwrite the
/// memory with a conversation nobody is looking at.
pub(super) fn use_remember_conversation(conversation_id: &str, active: bool) {
    let pump = use_pump();
    let core = pump.core();
    let conversation_id = conversation_id.to_owned();
    use_effect(use_reactive(
        (&conversation_id, &active),
        move |(conversation_id, active)| {
            if !active {
                return;
            }
            core.borrow()
                .storage()
                .set(LAST_CONVERSATION_KEY, &conversation_id);
            tracing::debug!(
                target: "agent_chat",
                conversation_id = %conversation_id,
                "last agent conversation remembered"
            );
        },
    ));
}
