//! Starts the agent-status owners over the session stack: the pinned
//! manifests, the ONE reference admission gate, and
//! [`AgentStatusStack::start`] with the stack's table, hooks, clock, report
//! environment and durable sink. Ports the composition in v2
//! `apps/worker/src/main.ts:220-237`. Called by `runtime::owners` only, before
//! the link's own session-closed hook so the detector forgets a session first.

use std::sync::Arc;

use anyhow::Context;

use super::session_stack::SessionStack;
use crate::agents::manifests::AgentManifests;
use crate::agents::reference_admission::AgentReferenceAdmissionGate;
use crate::agents::status_stack::{AgentStatusStack, AgentStatusStackDeps};
use crate::uplink::Uplink;

/// Build and start the agent-status owners. Must run inside the worker's
/// runtime.
pub fn start_agent_status(
    stack: &SessionStack,
    uplink: &Uplink,
) -> anyhow::Result<AgentStatusStack> {
    let manifests =
        AgentManifests::pinned().context("the pinned agent manifests do not compile")?;
    AgentStatusStack::start(AgentStatusStackDeps {
        uplink: uplink.clone(),
        table: Arc::clone(&stack.table),
        manager: &stack.manager,
        terminal_changed: &stack.terminal_changed,
        clock: stack.clock.clone(),
        manifests: Arc::new(manifests),
        environment: Arc::clone(&stack.agent_environment),
        reference_sink: stack.manager.durable_event_sink(),
        reference_admission: AgentReferenceAdmissionGate::new(),
        runtime: tokio::runtime::Handle::current(),
    })
    .context("the agent-status registry could not open an epoch")
}
