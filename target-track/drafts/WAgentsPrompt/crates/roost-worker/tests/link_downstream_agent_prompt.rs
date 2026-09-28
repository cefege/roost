//! The `agentPrompt` arm routes to the agent-prompt owner synchronously and
//! answers once, fenced; an owner that fails answers ambiguous with a STATIC
//! reason and logs nothing of the failure, because a dependency's error text
//! can carry the prompt or the status message. Ports
//! `apps/worker/tests/transport/coord-link-agent-prompt.test.ts` (the
//! no-owner case is `link_downstream_absent.rs`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;
#[path = "agent_prompt_support/log_capture.rs"]
mod log_capture;

use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode, SESSION, next_uplink, settle_tasks};
use roost_proto::DAgentPrompt;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, TerminalInputStatus, TerminalWritePhase,
};
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::channel;

const SECRET: &str = "prompt-and-status-secret-from-dependency";

fn prompt(request_id: &str, text: &str) -> Down {
    Down::AgentPrompt(DAgentPrompt {
        request_id: request_id.to_owned(),
        session_id: SESSION.to_owned(),
        input_seq: 7,
        expected_status_epoch: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
        expected_occupant_id: "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb".to_owned(),
        expected_revision: 1,
        text: text.to_owned(),
        budget_ms: 5_000,
        ..Default::default()
    })
}

#[tokio::test]
async fn an_agent_prompt_reaches_its_owner_synchronously_and_is_answered_once() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "process-epoch", Some(fakes.owners()));
    let mut link = FakeLink::default();
    dispatcher.dispatch(prompt("transport-agent-prompt", "go"), Instant::now(), &mut link);
    assert_eq!(fakes.log.calls(), ["agent_prompt.write_prompt:transport-agent-prompt"], "the owner is called synchronously");

    let Up::InputResult(result) = next_uplink(&mut receiver).await else { panic!("an input-result") };
    assert_eq!(result.request_id, "transport-agent-prompt");
    assert_eq!(result.input_seq, 7);
    assert_eq!((result.status, result.phase, result.written_bytes), (TerminalInputStatus::Accepted, TerminalWritePhase::Written, 3));
    assert!(result.reason.is_empty());
    settle_tasks().await;
    assert!(receiver.try_recv().is_none(), "exactly one answer");
    assert!(link.replies.is_empty(), "the owner's answer is fenced, not an immediate reply");
}

#[tokio::test]
async fn an_owner_failure_answers_a_static_ambiguous_result_and_logs_none_of_it() {
    let (logged, _capture) = log_capture::capture_events();
    let fakes = Fakes::new(OwnerMode::Panic);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "process-epoch", Some(fakes.owners()));
    let mut link = FakeLink::default();
    dispatcher.dispatch(prompt("failing-agent-prompt", SECRET), Instant::now(), &mut link);

    let Up::InputResult(result) = next_uplink(&mut receiver).await else { panic!("an input-result") };
    assert_eq!(result.request_id, "failing-agent-prompt");
    assert_eq!((result.status, result.phase, result.written_bytes), (TerminalInputStatus::Ambiguous, TerminalWritePhase::Unknown, 0));
    assert_eq!(result.reason, "worker agent prompt handler failed");
    let lines = logged();
    assert!(lines.iter().any(|line| line.starts_with("agent_prompt_failed ")), "{lines:?}");
    assert!(!lines.join("\n").contains(SECRET), "the dependency's error text reached the log");
}
