//! `SessionsPrompt` boundary: device authorization, exact validation,
//! non-oracular session rejection, dedicated worker framing, bounded public
//! outcomes, and rejection-cause classification -- including every worker
//! fence refusal (stale epoch, wrong occupant, stale revision, process proof).
//! Ports `apps/coord/tests/agents/agent-prompt-handlers.test.ts`.

// Integration tests may unwrap: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_prompt_support;
mod db_support;

use std::sync::Arc;

use agent_prompt_support::{
    FOREIGN_SESSION, MISSING_SESSION, PROMPT_OCCUPANT, PROMPT_SESSION, PROMPT_STATUS_EPOCH,
    PromptHarness, answer, request, result,
};
use connectrpc::ErrorCode;
use roost_coord::agents::rpc_prompt::handle_sessions_prompt;
use roost_coord::auth::principal::Principal;
use roost_proto::buffa::EnumValue;
use roost_proto::{AgentPromptInputOutcome, AgentPromptRejection, SessionsPromptRequest};
use roost_protocol::terminal_input::AGENT_PROMPT_MAX_WRITE_BYTES;
use roost_protocol::wire::coord_worker::{TerminalInputStatus as S, TerminalWritePhase as P};

fn outcome(response: &roost_proto::SessionsPromptResponse) -> Option<AgentPromptInputOutcome> {
    response.input_outcome.as_known()
}

fn rejection(response: &roost_proto::SessionsPromptResponse) -> Option<AgentPromptRejection> {
    response.rejection.as_ref().and_then(EnumValue::as_known)
}

// v2: "requires an existing dashboard device actor" -- a machine key is not one.
#[tokio::test]
async fn requires_an_account_device_actor() {
    let harness = PromptHarness::new("anon").await;
    let mut caller = harness.caller();
    caller.principal = Principal::Worker {
        fingerprint: agent_prompt_support::PROMPT_WORKER.to_owned(),
        label: "w".to_owned(),
    };
    let error = handle_sessions_prompt(&harness.core, &caller, request())
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unauthenticated);
}

// v2: "rejects every malformed identity, revision, text, and wait combination"
#[tokio::test]
async fn rejects_every_malformed_identity_revision_text_and_wait_combination() {
    let harness = PromptHarness::new("invalid").await;
    let with = |edit: fn(&mut SessionsPromptRequest)| {
        let mut req = request();
        edit(&mut req);
        req
    };
    let cases = [
        with(|r| r.session_id = "not-a-uuid".into()),
        with(|r| r.expected_status_epoch = "not-a-uuid".into()),
        with(|r| r.expected_occupant_id = "not-a-uuid".into()),
        with(|r| r.expected_revision = 9_007_199_254_740_992),
        with(|r| r.text = String::new()),
        with(|r| r.text = "é".repeat(8_193)),
        with(|r| r.wait_states = vec!["idle".into()]),
        with(|r| r.wait_timeout_ms = Some(1_000)),
        with(|r| {
            (r.wait_states, r.wait_timeout_ms) = (vec!["idle".into(), "idle".into()], Some(1_000))
        }),
        with(|r| (r.wait_states, r.wait_timeout_ms) = (vec!["done".into()], Some(1_000))),
        with(|r| (r.wait_states, r.wait_timeout_ms) = (vec!["idle".into()], Some(0))),
        with(|r| (r.wait_states, r.wait_timeout_ms) = (vec!["idle".into()], Some(300_001))),
    ];
    for (index, invalid) in cases.into_iter().enumerate() {
        let error = handle_sessions_prompt(&harness.core, &harness.caller(), invalid)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "case {index}");
        assert_eq!(
            error.message.as_deref(),
            Some("invalid agent prompt request")
        );
    }
    assert_eq!(harness.wait_count(), 0);
}

// v2: "makes absent and unrouted sessions the same definite rejection before
// wait or send" -- the unrouted session's worker is offline.
#[tokio::test]
async fn absent_and_offline_worker_sessions_are_the_same_rejection_before_wait_or_send() {
    let harness = PromptHarness::new("absent").await;
    harness.attach_worker(answer(S::Accepted, P::Written, 9, ""));
    for session in [MISSING_SESSION, FOREIGN_SESSION] {
        let mut req = request();
        req.session_id = session.to_owned();
        req.wait_states = vec!["idle".into()];
        req.wait_timeout_ms = Some(300_000);
        let response = handle_sessions_prompt(&harness.core, &harness.caller(), req)
            .await
            .unwrap()
            .body;
        assert_eq!(
            outcome(&response),
            Some(AgentPromptInputOutcome::Rejected),
            "{session}"
        );
        assert_eq!(response.written_bytes, 0);
        assert_eq!(response.reason, "agent prompt rejected");
        assert!(response.wait_outcome.is_none());
        assert_eq!(
            rejection(&response),
            Some(AgentPromptRejection::SessionUnavailable)
        );
    }
    assert!(harness.prompts().is_empty());
    assert_eq!(harness.wait_count(), 0);
}

// v2: "sends the exact dedicated status fence with a relative budget"
#[tokio::test]
async fn sends_the_exact_dedicated_status_fence_with_a_relative_budget() {
    let harness = PromptHarness::new("fence").await;
    harness.attach_worker(answer(S::Accepted, P::Written, 33, ""));
    let mut req = request();
    req.text = "first line\nsecond line".to_owned();
    let response = handle_sessions_prompt(&harness.core, &harness.caller(), req)
        .await
        .unwrap()
        .body;

    let prompts = harness.prompts();
    assert_eq!(prompts.len(), 1);
    let prompt = &prompts[0];
    assert_eq!(prompt.session_id, PROMPT_SESSION);
    assert_eq!(prompt.expected_status_epoch, PROMPT_STATUS_EPOCH);
    assert_eq!(prompt.expected_occupant_id, PROMPT_OCCUPANT);
    assert_eq!(prompt.expected_revision, 1);
    assert_eq!(prompt.text, "first line\nsecond line");
    assert!(!prompt.request_id.is_empty());
    assert!(prompt.input_seq > 0);
    assert!(prompt.budget_ms > 0);
    assert_eq!(outcome(&response), Some(AgentPromptInputOutcome::Accepted));
    assert_eq!(response.written_bytes, 33);
    assert_eq!(response.reason, "");
    assert!(response.wait_outcome.is_none());
}

// v2: "classifies every worker status/phase combination without retry"
#[tokio::test]
async fn classifies_every_worker_status_and_phase_combination_without_retry() {
    let harness = PromptHarness::new("classify").await;
    let over = AGENT_PROMPT_MAX_WRITE_BYTES as u32 + 1;
    let cases = [
        (
            S::Accepted,
            P::Written,
            13,
            AgentPromptInputOutcome::Accepted,
        ),
        (
            S::Accepted,
            P::PreWrite,
            13,
            AgentPromptInputOutcome::Ambiguous,
        ),
        (
            S::Rejected,
            P::PreWrite,
            0,
            AgentPromptInputOutcome::Rejected,
        ),
        (
            S::Rejected,
            P::Written,
            0,
            AgentPromptInputOutcome::Ambiguous,
        ),
        (
            S::Ambiguous,
            P::Unknown,
            4,
            AgentPromptInputOutcome::Ambiguous,
        ),
        (
            S::Accepted,
            P::Written,
            over,
            AgentPromptInputOutcome::Ambiguous,
        ),
        (
            S::Rejected,
            P::PreWrite,
            1,
            AgentPromptInputOutcome::Ambiguous,
        ),
    ];
    for (status, phase, bytes, expected) in cases {
        harness.attach_worker(answer(status, phase, bytes, "private worker status detail"));
        let response = handle_sessions_prompt(&harness.core, &harness.caller(), request())
            .await
            .unwrap()
            .body;
        assert_eq!(
            outcome(&response),
            Some(expected),
            "{status:?}/{phase:?}/{bytes}"
        );
        assert!(response.written_bytes as usize <= AGENT_PROMPT_MAX_WRITE_BYTES);
        assert!(!response.reason.contains("private worker status detail"));
        assert!(response.wait_outcome.is_none());
    }
    assert_eq!(harness.prompts().len(), cases.len());
}

// v2: "never emits prompt text or retained status messages in response or logs"
// (the response half; logs carry only ids and outcome words by construction).
#[tokio::test]
async fn never_emits_prompt_text_or_status_messages_in_the_response() {
    let harness = PromptHarness::new("secret").await;
    harness.retain_status("working", 2, Some("STATUS_SECRET_921ec3"), 0);
    harness.attach_worker(Arc::new(|_core, prompt| {
        Some(result(
            prompt,
            S::Rejected,
            P::PreWrite,
            0,
            "PROMPT_SECRET_7ac142:STATUS_SECRET_921ec3",
        ))
    }));
    let mut req = request();
    req.text = "PROMPT_SECRET_7ac142".to_owned();
    req.expected_revision = 2;
    let response = handle_sessions_prompt(&harness.core, &harness.caller(), req)
        .await
        .unwrap()
        .body;
    assert_eq!(response.reason, "agent prompt rejected");
    let public = format!("{response:?}");
    assert!(!public.contains("PROMPT_SECRET_7ac142"));
    assert!(!public.contains("STATUS_SECRET_921ec3"));
}

// v2: "gives every worker rejection cause its own bounded member". The fence
// cases are the worker's refusals for a stale epoch, a replaced occupant or a
// stale revision ("agent status fence changed"), and a process-proof mismatch.
#[tokio::test]
async fn gives_every_worker_rejection_cause_its_own_bounded_member() {
    let harness = PromptHarness::new("causes").await;
    let cases = [
        ("agent is blocked", Some(AgentPromptRejection::Blocked)),
        (
            "agent state does not admit prompts",
            Some(AgentPromptRejection::NotPromptable),
        ),
        (
            "agent status source is not integration",
            Some(AgentPromptRejection::NotPromptable),
        ),
        (
            "agent is not the terminal foreground process",
            Some(AgentPromptRejection::NotForeground),
        ),
        (
            "agent status fence changed",
            Some(AgentPromptRejection::FenceChanged),
        ),
        (
            "agent status is unavailable",
            Some(AgentPromptRejection::FenceChanged),
        ),
        (
            "agent process proof changed before the keeper write",
            Some(AgentPromptRejection::ProcessChanged),
        ),
        (
            "agent process proof changed before prompt admission",
            Some(AgentPromptRejection::ProcessChanged),
        ),
        (
            "agent process proof could not be refreshed",
            Some(AgentPromptRejection::ProcessChanged),
        ),
        (
            "session changed before the keeper write",
            Some(AgentPromptRejection::SessionUnavailable),
        ),
        ("prompt budget expired", Some(AgentPromptRejection::Expired)),
        (
            "prompt budget cannot cover the submit delay",
            Some(AgentPromptRejection::Expired),
        ),
        (
            "keeper rejected the agent prompt",
            Some(AgentPromptRejection::KeeperRejected),
        ),
        ("a cause this coordinator cannot classify", None),
    ];
    for (reason, expected) in cases {
        harness.attach_worker(answer(S::Rejected, P::PreWrite, 0, reason));
        let response = handle_sessions_prompt(&harness.core, &harness.caller(), request())
            .await
            .unwrap()
            .body;
        assert_eq!(
            outcome(&response),
            Some(AgentPromptInputOutcome::Rejected),
            "{reason}"
        );
        assert_eq!(rejection(&response), expected, "{reason}");
        assert_eq!(response.reason, "agent prompt rejected");
    }
}

// v2: "leaves an accepted or ambiguous outcome without a rejection member"
#[tokio::test]
async fn leaves_an_accepted_or_ambiguous_outcome_without_a_rejection_member() {
    let harness = PromptHarness::new("members").await;
    for (status, phase, bytes) in [(S::Accepted, P::Written, 9), (S::Ambiguous, P::Unknown, 4)] {
        harness.attach_worker(answer(status, phase, bytes, "agent is blocked"));
        let response = handle_sessions_prompt(&harness.core, &harness.caller(), request())
            .await
            .unwrap()
            .body;
        assert!(response.rejection.is_none(), "{status:?}");
    }
}
