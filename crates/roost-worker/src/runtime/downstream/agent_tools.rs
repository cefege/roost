//! Coordinator-dispatched tools run on the worker that owns the project files.
//!
//! The link dispatcher hands requests to `AgentToolsOwner`; output and terminal
//! results travel back through the worker link's uplink.

use roost_proto::{
    DAgentConversationClosed, DAgentJobsAbort, DAgentToolBackground, DAgentToolCall,
    DAgentToolCancel,
};

use super::Dispatcher;

impl Dispatcher {
    pub(super) fn agent_tool_call(&self, request: DAgentToolCall) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tools.as_ref())
        {
            owner.call(request);
        }
    }

    pub(super) fn agent_tool_cancel(&self, request: DAgentToolCancel) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tools.as_ref())
        {
            owner.cancel(request);
        }
    }

    pub(super) fn agent_tool_background(&self, request: DAgentToolBackground) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tools.as_ref())
        {
            owner.background_call(request);
        }
    }

    pub(super) fn agent_jobs_abort(&self, request: DAgentJobsAbort) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tools.as_ref())
        {
            owner.abort_jobs(request);
        }
    }

    pub(super) fn agent_conversation_closed(&self, request: DAgentConversationClosed) {
        if let Some(owner) = self
            .owners
            .as_ref()
            .and_then(|owners| owners.agent_tools.as_ref())
        {
            owner.close_conversation(request);
        }
    }
}
