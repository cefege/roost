//! The `agentPrompt` downstream arm: a worker without owners rejects it now,
//! pre-write; otherwise the agent-prompt owner answers once, fenced to the
//! connection the prompt arrived on. Called by [`super::Dispatcher::dispatch`].
//! Ports the `agentPrompt` case of v2
//! `apps/worker/src/transport/coord-link-downstream.ts`.

use std::sync::Arc;
use std::time::Instant;

use roost_proto::DAgentPrompt;
use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, TerminalInputStatus};

use super::owner_task::run_owner;
use super::replies;
use super::{Dispatcher, DownstreamLink};
use crate::uplink::RequestBudget;
use crate::uplink::terminal_results::InputResultKey;

impl Dispatcher {
    /// An owner that fails answers ambiguous with a STATIC reason, and its
    /// log line names the request and never the failure: a dependency's error
    /// text can carry the prompt or the status message, which never leave the
    /// worker.
    pub(super) fn agent_prompt(
        &self,
        request: DAgentPrompt,
        received: Instant,
        link: &mut dyn DownstreamLink,
    ) {
        let key = InputResultKey::from(&request);
        let Some(owners) = self.owners() else {
            let refusal = replies::input_result(
                &key,
                TerminalInputStatus::Rejected,
                replies::AGENT_PROMPT_HANDLER_UNAVAILABLE,
            );
            if let Some(frame) = refusal {
                link.reply(frame);
            }
            return;
        };
        let budget = RequestBudget::from_budget_ms(request.budget_ms, received);
        let fence = self.uplink.fence();
        let reply_fence = fence.clone();
        let uplink = self.uplink.clone();
        let occupant_id = request.expected_occupant_id.clone();
        let prompt = Arc::clone(&owners.agent_prompt);
        run_owner(
            move || prompt.write_prompt(request, budget, fence),
            Box::new(move |outcome| {
                let frame = match outcome {
                    Ok(Some(result)) => Some(CoordWorkerUpstream::InputResult(result)),
                    Ok(None) => {
                        tracing::warn!(request_id = %key.request_id, "the agent prompt owner has no result the wire can carry");
                        None
                    }
                    Err(_) => {
                        tracing::warn!(
                            request_id = %key.request_id,
                            session = %key.session_id,
                            occupant_id = %occupant_id,
                            outcome = "ambiguous",
                            "agent_prompt_failed"
                        );
                        replies::input_result(
                            &key,
                            TerminalInputStatus::Ambiguous,
                            replies::AGENT_PROMPT_HANDLER_FAILED,
                        )
                    }
                };
                if let Some(frame) = frame {
                    uplink.send_fenced(&reply_fence, frame);
                }
            }),
        );
    }
}
