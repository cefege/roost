//! Native admission and advisor hooks around the reusable generation task.
//! Host commands are imported at each actual request boundary; Durable still
//! owns context derivation, tool rounds, retries and commit/recovery state.

use std::sync::Arc;

use async_trait::async_trait;
use rpi_durable::Context;
use rpi_durable::harness::types::{
    Agent, ContinueInput, GenerationHooks, HookApi, RequestMessages,
};

use super::journal_sync::JournalSync;
use crate::runtime::AgentRuntime;

#[derive(Clone)]
pub(super) struct NativeHooks {
    pub runtime: AgentRuntime,
    pub conversation_id: String,
    pub sync: Arc<JournalSync>,
    pub projection: Arc<super::state::ProjectionState>,
    pub registry: std::sync::Weak<rpi_durable::harness::RegistryImpl>,
}

#[async_trait]
impl GenerationHooks for NativeHooks {
    async fn prepare_agent(
        &self,
        _agent: &Agent,
        api: &dyn HookApi,
        context: &Context,
    ) -> rpi_durable::session::SessionResult<Option<Agent>> {
        let registry = self.registry.upgrade().ok_or_else(|| {
            rpi_durable::session::SessionError::message("Native registry is closed")
        })?;
        super::driver::install_extension(
            &self.runtime,
            &self.conversation_id,
            &registry,
            &self.projection,
            Arc::new(self.clone()),
        )
        .await
        .map_err(|error| rpi_durable::session::SessionError::message(error.to_string()))?;
        let agent = self
            .sync
            .current_agent(api.conversation_id(), context)
            .await?;
        super::policy::prepare(
            &self.runtime,
            &self.conversation_id,
            &self.projection,
            agent,
            api,
            context,
        )
        .await
    }

    async fn before_request_gate(
        &self,
        _request: &RequestMessages,
        _api: &dyn HookApi,
        context: &Context,
    ) -> rpi_durable::session::SessionResult<Option<RequestMessages>> {
        self.sync
            .dispatch_boundary(context)
            .await
            .map(|messages| Some(RequestMessages { messages }))
            .map_err(|error| match error {
                crate::error::AgentError::Cancelled => rpi_durable::session::SessionError::Aborted,
                other => rpi_durable::session::SessionError::message(other.to_string()),
            })
    }

    async fn before_request(
        &self,
        _request: RequestMessages,
        _api: &dyn HookApi,
        _context: &Context,
    ) -> Option<RequestMessages> {
        None
    }

    async fn after_response(
        &self,
        _message: &rpi_ai::types::AssistantMessage,
        _api: &dyn HookApi,
        _context: &Context,
    ) {
    }

    async fn on_yield(
        &self,
        _answer: &rpi_ai::types::AssistantMessage,
        _api: &dyn HookApi,
        _context: &Context,
    ) -> Option<ContinueInput> {
        None
    }

    async fn after_tools(
        &self,
        _assistant: rpi_durable::EntryId,
        _results: &[rpi_durable::EntryId],
        _api: &dyn HookApi,
        _context: &Context,
    ) {
    }
}
