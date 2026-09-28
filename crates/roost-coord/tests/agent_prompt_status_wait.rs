//! `SessionsPrompt`'s status-wait arm: which agent transitions satisfy a
//! requested wait, when a prompted idle agent stalls instead of matching, and
//! that every waiter is consumed. Drives the real hub and waiter table.
//! Ports `apps/coord/tests/agents/agent-prompt-handlers-status-wait.test.ts`.

// Integration tests may unwrap: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_prompt_support;

use std::sync::Arc;

use agent_prompt_support::{PromptHarness, request, result, retain_status};
use roost_coord::agents::rpc_prompt::handle_sessions_prompt;
use roost_proto::{AgentPromptInputOutcome, AgentPromptWaitOutcome, SessionsPromptRequest};
use roost_protocol::wire::coord_worker::{TerminalInputStatus as S, TerminalWritePhase as P};

fn waiting(states: &[&str], timeout_ms: u32, revision: u64) -> SessionsPromptRequest {
    let mut req = request();
    req.wait_states = states.iter().map(|s| (*s).to_owned()).collect();
    req.wait_timeout_ms = Some(timeout_ms);
    req.expected_revision = revision;
    req
}

async fn prompt(
    harness: &PromptHarness,
    req: SessionsPromptRequest,
) -> (Option<AgentPromptInputOutcome>, Option<AgentPromptWaitOutcome>) {
    let response = handle_sessions_prompt(&harness.core, &harness.caller(), req).await.unwrap().body;
    (
        response.input_outcome.as_known(),
        response.wait_outcome.as_ref().and_then(|w| w.as_known()),
    )
}

// v2: "captures a fast accepted transition emitted during worker send"
#[tokio::test]
async fn captures_a_fast_accepted_transition_emitted_during_worker_send() {
    let harness = PromptHarness::new("fast").await;
    harness.attach_worker(Arc::new(|core, prompt| {
        retain_status(core, "idle", 2, None, 2);
        Some(result(prompt, S::Accepted, P::Written, 9, ""))
    }));
    let outcome = prompt(&harness, waiting(&["idle"], 30_000, 1)).await;
    assert_eq!(
        outcome,
        (Some(AgentPromptInputOutcome::Accepted), Some(AgentPromptWaitOutcome::Matched))
    );
    assert_eq!(harness.wait_count(), 0);
}

// v2: "awaits the requested state even when input completion is ambiguous"
#[tokio::test]
async fn awaits_the_requested_state_even_when_input_completion_is_ambiguous() {
    let harness = PromptHarness::new("ambiguous").await;
    harness.attach_worker(Arc::new(|core, prompt| {
        retain_status(core, "blocked", 2, None, 0);
        Some(result(prompt, S::Ambiguous, P::Unknown, 2, ""))
    }));
    let outcome = prompt(&harness, waiting(&["blocked"], 30_000, 1)).await;
    assert_eq!(
        outcome,
        (Some(AgentPromptInputOutcome::Ambiguous), Some(AgentPromptWaitOutcome::Matched))
    );
    assert_eq!(harness.wait_count(), 0);
}

// v2: "stalls instead of matching when a prompted idle agent never starts a turn".
// Real time: the five-second activity gate elapses once.
#[tokio::test]
async fn stalls_when_a_prompted_idle_agent_never_starts_a_turn() {
    let harness = PromptHarness::new("stall").await;
    harness.retain_status("idle", 2, None, 2);
    harness.attach_worker(Arc::new(|core, prompt| {
        // Same idle state, higher revision: only the status message changed.
        retain_status(core, "idle", 3, Some("waiting for you"), 2);
        Some(result(prompt, S::Accepted, P::Written, 9, ""))
    }));
    let outcome = prompt(&harness, waiting(&["idle"], 30_000, 2)).await;
    assert_eq!(
        outcome,
        (Some(AgentPromptInputOutcome::Accepted), Some(AgentPromptWaitOutcome::PromptStalled))
    );
    assert_eq!(harness.wait_count(), 0);
}

// v2: "matches once a prompted idle agent works and then completes"
#[tokio::test]
async fn matches_once_a_prompted_idle_agent_works_and_then_completes() {
    let harness = PromptHarness::new("works").await;
    harness.retain_status("idle", 2, None, 2);
    harness.attach_worker(Arc::new(|core, prompt| {
        retain_status(core, "working", 3, None, 2);
        retain_status(core, "idle", 4, None, 4);
        Some(result(prompt, S::Accepted, P::Written, 9, ""))
    }));
    let outcome = prompt(&harness, waiting(&["idle"], 30_000, 2)).await;
    assert_eq!(
        outcome,
        (Some(AgentPromptInputOutcome::Accepted), Some(AgentPromptWaitOutcome::Matched))
    );
    assert_eq!(harness.wait_count(), 0);
}

// v2: "aborts and consumes the provisional waiter on a definite rejection"
#[tokio::test]
async fn consumes_the_provisional_waiter_on_a_definite_rejection() {
    let harness = PromptHarness::new("reject").await;
    harness.attach_worker(Arc::new(|_core, prompt| {
        Some(result(prompt, S::Rejected, P::PreWrite, 0, ""))
    }));
    let outcome = prompt(&harness, waiting(&["idle"], 300_000, 1)).await;
    assert_eq!(outcome, (Some(AgentPromptInputOutcome::Rejected), None));
    assert_eq!(harness.wait_count(), 0);
}
