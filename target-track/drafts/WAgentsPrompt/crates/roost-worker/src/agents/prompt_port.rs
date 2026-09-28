//! The coordinator link's agent-prompt owner: the shared input work budget,
//! the status-fenced control, and the one `input-result` it answers with.
//! Implements `link_ports::AgentPromptPort` for `runtime::downstream`; built by
//! `runtime::owners` over the session layer and the agent status stack. Ports
//! `onAgentPrompt` of `apps/worker/src/transport/coord-link-deps.ts`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use roost_proto::DAgentPrompt;
use roost_protocol::terminal_input::{BRACKETED_PASTE_END, BRACKETED_PASTE_START, CR_BYTES};
use roost_protocol::wire::brand::SessionId;
use roost_protocol::wire::coord_worker::InputResult;

use super::detector::AgentScreenDetector;
use super::process_scan::{AgentProcessIdentity, ScanAbort};
use super::prompt_control::{
    AgentProcessProver, AgentPromptControlDeps, AgentStatusProofs, write_agent_prompt,
};
use super::prompt_fence::PromptBudget;
use super::registry::{AgentStatusPrivateProof, AgentStatusRegistry};
use crate::link_ports::AgentPromptPort;
use crate::session::input_write::WorkerInputResult;
use crate::terminal_input::{InputWorkOrigin, TerminalInputWorkBudget};
use crate::uplink::terminal_results::{InputResultKey, worker_input_result};
use crate::uplink::{LinkFence, OwnerFuture, RequestBudget};

/// v2's `onAgentPrompt`, over the one work budget every browser-triggered
/// terminal write shares.
#[derive(Debug)]
pub struct AgentPromptOwner {
    deps: Arc<AgentPromptControlDeps>,
    work_budget: TerminalInputWorkBudget,
}

impl AgentPromptOwner {
    pub fn new(deps: AgentPromptControlDeps, work_budget: TerminalInputWorkBudget) -> Self {
        Self {
            deps: Arc::new(deps),
            work_budget,
        }
    }
}

impl AgentPromptPort for AgentPromptOwner {
    /// The reservation covers the framed write (text, both paste brackets and
    /// the CR) and is taken before anything else, then held until the result
    /// exists, so a flood of prompts is refused pre-write rather than queued.
    fn write_prompt(
        &self,
        request: DAgentPrompt,
        budget: RequestBudget,
        fence: LinkFence,
    ) -> OwnerFuture<Option<InputResult>> {
        let key = InputResultKey::from(&request);
        let framed_bytes = request.text.len()
            + BRACKETED_PASTE_START.len()
            + BRACKETED_PASTE_END.len()
            + CR_BYTES.len();
        let reservation = match self.work_budget.reserve_input(&InputWorkOrigin::Sync, framed_bytes) {
            Ok(reservation) => reservation,
            Err(reason) => {
                let refused = WorkerInputResult::Rejected {
                    reason: reason.to_owned(),
                };
                return Box::pin(std::future::ready(worker_input_result(&key, &refused, true)));
            }
        };
        let deps = Arc::clone(&self.deps);
        let budget = LinkPromptBudget { budget, fence };
        Box::pin(async move {
            let result = write_agent_prompt(&request, &budget, &deps).await;
            drop(reservation);
            worker_input_result(&key, &result, true)
        })
    }
}

/// A request budget measured from frame receipt, fenced to the connection the
/// prompt arrived on. v2 `terminalBudget(socket, request.budgetMs)`.
#[derive(Debug, Clone)]
struct LinkPromptBudget {
    budget: RequestBudget,
    fence: LinkFence,
}

impl PromptBudget for LinkPromptBudget {
    fn is_current_connection(&self) -> bool {
        self.fence.is_current()
    }

    fn remaining(&self) -> Duration {
        self.budget.remaining(Instant::now())
    }
}

impl AgentStatusProofs for AgentStatusRegistry {
    fn current_private_proof(&self, session_id: &SessionId) -> Option<AgentStatusPrivateProof> {
        AgentStatusRegistry::current_private_proof(self, session_id)
    }
}

impl AgentProcessProver for AgentScreenDetector {
    fn reporting_agent_for_session(
        &self,
        session_id: &SessionId,
        reporter_pid: u32,
        abort: Option<ScanAbort>,
    ) -> OwnerFuture<Option<AgentProcessIdentity>> {
        AgentScreenDetector::reporting_agent_for_session(self, session_id, reporter_pid, abort)
    }
}
