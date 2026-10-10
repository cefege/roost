//! The asynchronous advisor shadow for primary conversations.
//! `hub` serializes reviews; focused modules own rendering, emission policy,
//! quarantine, delivery, and slash-command reporting.

mod command;
mod delivery;
mod delta;
mod guard;
mod hub;
mod quarantine;
mod review;

use crate::error::AgentError;
use crate::records::ConversationRecord;
use crate::runtime::AgentRuntime;

pub(crate) use hub::AdvisorHub;

impl AgentRuntime {
    pub(crate) fn advisor_turn_ended(&self, id: &str, ended_with_tool_calls: bool) {
        AdvisorHub::enqueue(self, id, ended_with_tool_calls);
    }

    pub(crate) fn advisor_reset(&self, id: &str) {
        AdvisorHub::reset(self, id);
    }

    pub(crate) fn advisor_forget(&self, id: &str) {
        AdvisorHub::forget(self, id);
    }
}

pub(crate) async fn command(
    runtime: &AgentRuntime,
    record: &ConversationRecord,
    args: &str,
) -> Result<(), AgentError> {
    command::command(runtime, record, args).await
}
