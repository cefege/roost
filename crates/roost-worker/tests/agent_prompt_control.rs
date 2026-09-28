//! Agent-prompt admission end to end over a real session manager and a scripted
//! keeper: the shared text encoder, keeper outcome truth, the downstream-hop
//! validation, the settle delay before the CR, and the lane it shares with raw
//! input. Ports `apps/worker/tests/agents/agent-prompt-control.test.ts`; the
//! fence races around the lane live in `agent_prompt_fences.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_prompt_support;
mod session_support;

use std::time::{Duration, Instant};

use agent_prompt_support::log_capture::capture_events;
use agent_prompt_support::{
    PromptHarness, ScriptedProver, TestBudget, eventually, process_proof, request_for,
};
use roost_protocol::terminal_input::AGENT_PROMPT_MAX_TEXT_BYTES;
use roost_worker::agents::prompt_control::write_agent_prompt;
use roost_worker::agents::prompt_submit::PROMPT_SUBMIT_DELAY;
use roost_worker::session::input_write::WorkerInputResult;
use roost_worker::session::keeper_channels::KeeperInputResult;
use roost_worker::uplink::TERMINAL_REQUEST_BUDGET_CAP_MS;
use session_support::input_script::ScriptedAnswer;
use session_support::{SESSION, session_id};

fn rejected(reason: &str) -> WorkerInputResult {
    WorkerInputResult::Rejected {
        reason: reason.to_owned(),
    }
}

#[tokio::test]
async fn the_prompt_uses_the_final_bracketed_paste_mode_raw_input_stays_byte_exact_and_logs_omit_the_text()
 {
    let (logged, _capture) = capture_events();
    let harness = PromptHarness::with_prover(|table| {
        ScriptedProver::answering(Box::new(move |call| {
            if call.call == 2 {
                // Between the two scans the application turns bracketed paste
                // on; the payload must be framed by the mode at write time.
                table.with_record_mut(&session_id(SESSION), |record| {
                    record.terminal_core.write(b"\x1b[?2004h");
                });
            }
            Box::pin(std::future::ready(Some(process_proof())))
        }))
    });
    let prompt_text = "prompt-secret\r\nsecond\x1bZ";
    let mut request = request_for(&harness.status_proof());
    request.text = prompt_text.to_owned();
    let prompt = write_agent_prompt(&request, &TestBudget::live(), &harness.deps).await;
    let raw = harness.spawn_raw(b"raw\n\x1b[31m").await.unwrap();

    let framed = "\x1b[200~prompt-secret\rsecondZ\x1b[201~";
    assert_eq!(
        prompt,
        WorkerInputResult::Accepted {
            written_bytes: (framed.len() + 1) as u32
        }
    );
    assert_eq!(raw, WorkerInputResult::Accepted { written_bytes: 9 });
    assert_eq!(harness.written(), [framed, "\r", "raw\n\x1b[31m"]);
    let lines = logged().join("\n");
    assert!(
        !lines.contains("prompt-secret"),
        "the prompt text reached the log: {lines}"
    );
}

/// The refusal a prompt must draw, and the edit that makes it invalid.
type InvalidCase = (&'static str, fn(&mut roost_proto::DAgentPrompt));

#[tokio::test]
async fn utf8_bytes_safe_revision_and_the_hop_budget_are_enforced_before_any_scan_or_write() {
    let harness = PromptHarness::new(ScriptedProver::proving());
    let proof = harness.status_proof();
    let budget = TestBudget::live();
    let invalid: [InvalidCase; 5] = [
        ("request_id is invalid", |request| {
            request.request_id.clear()
        }),
        ("session_id must be a UUID", |request| {
            request.session_id = "not-a-session".to_owned()
        }),
        ("input sequence must be a positive uint64", |request| {
            request.input_seq = 0
        }),
        ("expected_status_epoch must be a UUID", |request| {
            request.expected_status_epoch = "not-an-epoch".to_owned()
        }),
        ("expected_occupant_id must be a UUID", |request| {
            request.expected_occupant_id = "not-an-occupant".to_owned()
        }),
    ];
    for (reason, corrupt) in invalid {
        let mut request = request_for(&proof);
        corrupt(&mut request);
        assert_eq!(
            write_agent_prompt(&request, &budget, &harness.deps).await,
            rejected(reason)
        );
    }
    let mut over_cap = request_for(&proof);
    over_cap.text = "🐙".repeat(AGENT_PROMPT_MAX_TEXT_BYTES / 4 + 1);
    assert_eq!(
        write_agent_prompt(&over_cap, &budget, &harness.deps).await,
        rejected("prompt text is invalid")
    );
    let mut unsafe_revision = request_for(&proof);
    unsafe_revision.expected_revision = 1 << 53;
    assert_eq!(
        write_agent_prompt(&unsafe_revision, &budget, &harness.deps).await,
        rejected("expected_revision must be a safe uint64")
    );
    let mut over_budget = request_for(&proof);
    over_budget.budget_ms = TERMINAL_REQUEST_BUDGET_CAP_MS + 1;
    assert_eq!(
        write_agent_prompt(&over_budget, &budget, &harness.deps).await,
        rejected("budget_ms is invalid")
    );
    assert_eq!(harness.prover.calls(), 0);
    assert!(harness.written().is_empty());

    let mut at_cap = request_for(&proof);
    at_cap.text = "🐙".repeat(AGENT_PROMPT_MAX_TEXT_BYTES / 4);
    assert_eq!(
        write_agent_prompt(&at_cap, &budget, &harness.deps).await,
        WorkerInputResult::Accepted {
            written_bytes: (AGENT_PROMPT_MAX_TEXT_BYTES + 1) as u32
        }
    );
    assert_eq!(harness.prover.calls(), 2);
    assert_eq!(harness.written().len(), 2);
}

#[tokio::test]
async fn a_prompt_expires_behind_an_unresolved_predecessor_without_losing_receive_order() {
    let harness = PromptHarness::new(ScriptedProver::proving());
    let predecessor = harness.hold_lane().await;
    let budget = TestBudget::until(Instant::now() + Duration::from_millis(50));
    let prompt = harness.spawn_prompt(request_for(&harness.status_proof()), budget);
    eventually("the prompt queued behind the predecessor", || {
        harness.lane().0 == 2
    })
    .await;
    let raw = harness.spawn_raw(b"later-raw-input");

    assert_eq!(prompt.await.unwrap(), rejected("prompt budget expired"));
    assert_eq!(harness.prover.calls(), 1);
    assert!(harness.written().is_empty());
    predecessor.release();
    assert_eq!(
        raw.await.unwrap(),
        WorkerInputResult::Accepted { written_bytes: 15 }
    );
    assert_eq!(harness.written(), ["later-raw-input"]);
}

#[tokio::test]
async fn keeper_rejected_partial_and_unknown_truth_is_kept_without_a_retry() {
    let harness = PromptHarness::new(ScriptedProver::proving());
    let proof = harness.status_proof();
    let input = &harness.session.keeper.input;
    input.answer_next(ScriptedAnswer::Answered(KeeperInputResult::Reject {
        reason: "queue_full".to_owned(),
    }));
    let mut reject = request_for(&proof);
    reject.text = "reject".to_owned();
    assert_eq!(
        write_agent_prompt(&reject, &TestBudget::live(), &harness.deps).await,
        rejected("keeper rejected the agent prompt")
    );
    input.answer_next(ScriptedAnswer::Answered(KeeperInputResult::Ambiguous {
        written: Some(2),
        reason: "invalid_write_count".to_owned(),
    }));
    let mut partial = request_for(&proof);
    partial.text = "partial".to_owned();
    assert_eq!(
        write_agent_prompt(&partial, &TestBudget::live(), &harness.deps).await,
        WorkerInputResult::Ambiguous {
            written_bytes: 2,
            reason: "keeper agent prompt outcome is ambiguous".to_owned()
        }
    );
    input.answer_next(ScriptedAnswer::Answered(KeeperInputResult::Ambiguous {
        written: None,
        reason: "disconnected".to_owned(),
    }));
    let mut unknown = request_for(&proof);
    unknown.text = "unknown".to_owned();
    assert_eq!(
        write_agent_prompt(&unknown, &TestBudget::live(), &harness.deps).await,
        WorkerInputResult::Ambiguous {
            written_bytes: 0,
            reason: "keeper agent prompt outcome is ambiguous".to_owned()
        }
    );
    assert_eq!(harness.written(), ["reject", "partial", "unknown"]);
}

#[tokio::test]
async fn the_cr_goes_out_only_after_the_settle_delay_and_is_accepted_only_when_acknowledged() {
    let harness = PromptHarness::new(ScriptedProver::proving());
    let input = &harness.session.keeper.input;
    let text_answer = input.hold_next();
    input.answer_next(ScriptedAnswer::Answered(KeeperInputResult::Reject {
        reason: "queue_full".to_owned(),
    }));
    let mut request = request_for(&harness.status_proof());
    request.text = "draft".to_owned();
    let prompt = harness.spawn_prompt(request, TestBudget::live());
    eventually("the text batch reached the keeper", || {
        harness.written().len() == 1
    })
    .await;
    let acknowledged_at = Instant::now();
    text_answer
        .send(KeeperInputResult::Ack { written: 5 })
        .unwrap();
    eventually("the CR reached the keeper", || harness.written().len() == 2).await;
    assert!(
        acknowledged_at.elapsed() >= PROMPT_SUBMIT_DELAY,
        "the CR was written before the paste settled"
    );

    assert_eq!(
        prompt.await.unwrap(),
        WorkerInputResult::Ambiguous {
            written_bytes: 5,
            reason: "keeper did not submit the agent prompt".to_owned()
        }
    );
    assert_eq!(harness.written(), ["draft", "\r"]);
}

#[tokio::test]
async fn a_budget_that_cannot_cover_the_submit_delay_rejects_without_writing() {
    let harness = PromptHarness::new(ScriptedProver::proving());
    let budget = TestBudget::fixed(PROMPT_SUBMIT_DELAY);
    assert_eq!(
        write_agent_prompt(
            &request_for(&harness.status_proof()),
            &budget,
            &harness.deps
        )
        .await,
        rejected("prompt budget cannot cover the submit delay")
    );
    assert!(harness.written().is_empty());
    assert_eq!(harness.prover.calls(), 2);
}
