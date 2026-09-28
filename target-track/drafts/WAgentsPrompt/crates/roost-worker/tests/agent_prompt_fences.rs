//! The pre-write fences of agent-prompt admission and their races with the
//! keeper write lane: a blocked or screen-only status, the receive order a
//! queued prompt keeps, a rejected proof draining its ticket, a stalled final
//! scan aborted at budget expiry, a fence that moves while the prompt is
//! queued, and the final gates before the write. Ports
//! `apps/worker/tests/agents/agent-prompt-fences.test.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_prompt_support;
mod session_support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_prompt_support::{
    AGENT_PID, CHANNEL, PromptHarness, ScriptedProver, TestBudget, eventually, process_proof,
    request_for,
};
use roost_protocol::wire::agent_status::{AgentRuntimeState, AgentStatusSource};
use roost_worker::agents::BuiltinAgentId;
use roost_worker::agents::process_scan::{AgentProcessIdentity, ScanAbort};
use roost_worker::agents::prompt_control::write_agent_prompt;
use roost_worker::session::input_write::WorkerInputResult;
use tokio::sync::oneshot;

fn rejected(reason: &str) -> WorkerInputResult {
    WorkerInputResult::Rejected { reason: reason.to_owned() }
}

/// A prover whose FIRST scan answers only when the test says so.
fn first_scan_held(answer: Option<AgentProcessIdentity>) -> (Arc<ScriptedProver>, oneshot::Sender<()>) {
    let (release, held) = oneshot::channel::<()>();
    let held = Mutex::new(Some(held));
    let prover = ScriptedProver::answering(Box::new(move |call| {
        let answer = answer.clone();
        let wait = if call.call == 1 { held.lock().unwrap().take() } else { None };
        Box::pin(async move {
            if let Some(wait) = wait {
                let _ = wait.await;
            }
            answer
        })
    }));
    (prover, release)
}

#[tokio::test]
async fn blocked_and_screen_only_status_reject_without_a_process_refresh() {
    for (state, source, reason) in [
        (AgentRuntimeState::Blocked, AgentStatusSource::Integration, "agent is blocked"),
        (AgentRuntimeState::Idle, AgentStatusSource::Screen, "agent status source is not integration"),
    ] {
        let harness = PromptHarness::new(ScriptedProver::proving());
        let mut proof = harness.status_proof();
        proof.state = state;
        proof.source = source;
        harness.status.set(Some(proof.clone()));
        assert_eq!(
            write_agent_prompt(&request_for(&proof), &TestBudget::live(), &harness.deps).await,
            rejected(reason)
        );
        assert_eq!(harness.prover.calls(), 0);
    }
}

#[tokio::test]
async fn a_prompt_reserves_its_receive_order_before_the_initial_scan_settles() {
    let (prover, release_scan) = first_scan_held(Some(process_proof()));
    let harness = PromptHarness::new(prover);
    let prompt = harness.spawn_prompt(request_for(&harness.status_proof()), TestBudget::live());
    eventually("the initial scan started", || harness.prover.calls() == 1).await;
    assert_eq!(harness.lane().0, 1, "the prompt holds its place before its scan answers");
    let raw = harness.spawn_raw(b"later-raw-input");
    eventually("the raw input queued", || harness.lane().0 == 2).await;
    release_scan.send(()).unwrap();

    assert_eq!(prompt.await.unwrap(), WorkerInputResult::Accepted { written_bytes: 9 });
    assert_eq!(raw.await.unwrap(), WorkerInputResult::Accepted { written_bytes: 15 });
    assert_eq!(harness.written(), ["continue", "\r", "later-raw-input"]);
}

#[tokio::test]
async fn a_queued_ticket_drains_when_the_initial_process_proof_is_rejected() {
    let (prover, release_scan) = first_scan_held(None);
    let harness = PromptHarness::new(prover);
    let blocker = harness.hold_lane().await;
    let prompt = harness.spawn_prompt(request_for(&harness.status_proof()), TestBudget::live());
    eventually("the initial scan started", || harness.prover.calls() == 1).await;
    let raw = harness.spawn_raw(b"raw-after-rejection");
    eventually("blocker, prompt and raw input all queued", || harness.lane().0 == 3).await;
    release_scan.send(()).unwrap();

    assert_eq!(prompt.await.unwrap(), rejected("agent process proof could not be refreshed"));
    blocker.release();
    assert_eq!(raw.await.unwrap(), WorkerInputResult::Accepted { written_bytes: 19 });
    assert_eq!(harness.written(), ["raw-after-rejection"]);
    assert_eq!(harness.lane(), (0, None));
}

#[tokio::test]
async fn a_stalled_final_scan_is_aborted_and_input_released_at_budget_expiry() {
    let final_abort: Arc<Mutex<Option<ScanAbort>>> = Arc::default();
    let seen = Arc::clone(&final_abort);
    let harness = PromptHarness::new(ScriptedProver::answering(Box::new(move |call| {
        if call.call == 1 {
            return Box::pin(std::future::ready(Some(process_proof())));
        }
        *seen.lock().unwrap() = call.abort;
        Box::pin(std::future::pending())
    })));
    let started = Instant::now();
    let budget = TestBudget::until(started + Duration::from_millis(100));
    let prompt = harness.spawn_prompt(request_for(&harness.status_proof()), budget);
    eventually("the final scan started", || harness.prover.calls() == 2).await;
    let raw = harness.spawn_raw(b"raw-after-timeout");

    assert_eq!(prompt.await.unwrap(), rejected("prompt budget expired"));
    assert_eq!(raw.await.unwrap(), WorkerInputResult::Accepted { written_bytes: 17 });
    assert!(started.elapsed() < Duration::from_secs(1));
    let abort = final_abort.lock().unwrap().clone().expect("the final scan was handed an abort");
    assert!(abort.is_aborted(), "the stalled scan was not told to stop");
    assert_eq!(harness.written(), ["raw-after-timeout"]);
    assert_eq!(harness.lane(), (0, None));
}

#[tokio::test]
async fn a_stale_revision_a_replaced_occupant_and_a_closed_session_reject_after_queueing() {
    for (mutation, reason) in [
        ("revision", "agent status fence changed"),
        ("replacement", "agent status fence changed"),
        ("closed", "session changed before prompt admission"),
    ] {
        let (prover, release_scan) = first_scan_held(Some(process_proof()));
        let harness = PromptHarness::new(prover);
        let blocker = harness.hold_lane().await;
        let prompt = harness.spawn_prompt(request_for(&harness.status_proof()), TestBudget::live());
        eventually("the prompt queued", || harness.prover.calls() == 1 && harness.lane().0 == 2).await;
        match mutation {
            "revision" => harness.status.bump_revision(),
            "replacement" => harness.status.replace_occupant(),
            _ => {
                harness.status.set(None);
                harness.session.manager.close_channel(CHANNEL, None).await.unwrap();
            }
        }
        release_scan.send(()).unwrap();
        blocker.release();
        assert_eq!(prompt.await.unwrap(), rejected(reason), "{mutation}");
        assert_eq!(harness.prover.calls(), 1, "{mutation}");
        assert!(harness.written().is_empty(), "{mutation}");
    }
}

#[tokio::test]
async fn the_final_deadline_connection_and_refreshed_process_proof_all_gate_the_write() {
    for (failure, reason) in [
        ("deadline", "prompt budget expired"),
        ("connection", "worker connection was superseded"),
        ("process", "agent process proof changed before the keeper write"),
    ] {
        let budget = TestBudget::live();
        let moved = budget.clone();
        let harness = PromptHarness::new(ScriptedProver::answering(Box::new(move |call| {
            let answer = match (call.call, failure) {
                (2, "deadline") => {
                    moved.spend();
                    process_proof()
                }
                (2, "connection") => {
                    moved.supersede();
                    process_proof()
                }
                (2, _) => AgentProcessIdentity { agent_id: BuiltinAgentId::Omp, pid: AGENT_PID + 1, foreground: None },
                _ => process_proof(),
            };
            Box::pin(std::future::ready(Some(answer)))
        })));
        assert_eq!(
            write_agent_prompt(&request_for(&harness.status_proof()), &budget, &harness.deps).await,
            rejected(reason),
            "{failure}"
        );
        assert_eq!(harness.prover.calls(), 2, "{failure}");
        assert!(harness.written().is_empty(), "{failure}");
    }
}
