//! `/loop` against scripted seams: count and condition budgets, pause/off
//! transitions, journal recovery across runtime reconstruction, and races at
//! condition lookup and repeat-prompt commits.

#[path = "loop_mode/commit.rs"]
mod commit;
#[path = "loop_mode/recovery.rs"]
mod recovery;
#[path = "support/mod.rs"]
mod support;

use std::time::Duration;

use roost_agent::AgentError;
use roost_protocol::wire::agent_chat::{AgentRunState, TranscriptItem};
use support::{Harness, text_reply, tool_reply, user_texts};

const LIMIT_REACHED: &str = "Loop limit reached. Loop mode disabled.";

fn notice_bodies(items: &[TranscriptItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Notice { body, .. } => Some(body.clone()),
            _ => None,
        })
        .collect()
}

fn loop_status(items: &[TranscriptItem]) -> Option<String> {
    items.iter().find_map(|item| match item {
        TranscriptItem::LoopStatus { status, .. } => Some(status.clone()),
        _ => None,
    })
}

/// Makes the loop gate's `bash` check report an explicit exit code.
fn bash_gate_exit(harness: &Harness, exit_code: i64) {
    harness.tools.answer("bash", "gate");
    if let Some(outcome) = harness.tools.answers.lock().unwrap().get_mut("bash") {
        outcome.details_json = format!(r#"{{"exit_code":{exit_code}}}"#);
    }
}

/// Waits for a store seam to park; a loop that never reaches it fails the test.
async fn parked(reached: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(5), reached)
        .await
        .expect("the loop reached the parked point");
}

#[tokio::test]
async fn a_count_limit_repeats_the_prompt_then_disables_the_loop() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("first"));
    harness.llm.script(&id, text_reply("second"));
    harness.submit(&id, "/loop 2 keep going").await;
    let transcript = harness.settle(&id).await;

    assert_eq!(transcript.run_state, AgentRunState::Idle);
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    assert_eq!(user_texts(&requests[1]), vec!["keep going", "keep going"]);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn a_count_of_one_takes_the_next_prompt_as_its_only_run() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 1").await;
    harness.llm.script(&id, text_reply("done"));
    harness.submit(&id, "fix the build").await;
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn an_explicit_abort_pauses_the_loop_and_a_late_steer_does_not_restart_it() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    *harness.tools.delay.lock().unwrap() = Some(Duration::from_secs(30));
    harness.llm.script(
        &id,
        tool_reply(&[("call-1", "bash", r#"{"command":"make"}"#)]),
    );
    harness.submit(&id, "/loop 3 check the build").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    harness.submit(&id, "and also the tests").await;
    assert!(harness.runtime.abort(&id).await.unwrap());
    let transcript = harness.settle(&id).await;

    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert_eq!(
        harness.llm.requests_for(&id).len(),
        1,
        "an explicit abort must not restart the loop"
    );
    let late_steers = transcript
        .items
        .iter()
        .filter(|item| {
            matches!(item, TranscriptItem::User { text, .. } if text.as_str() == "and also the tests")
        })
        .count();
    assert_eq!(late_steers, 1);
    assert!(loop_status(&transcript.items).is_some_and(|status| status.contains("paused")));
}

#[tokio::test]
async fn a_prompt_after_an_abort_resumes_the_paused_loop() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    *harness.tools.delay.lock().unwrap() = Some(Duration::from_secs(30));
    harness.llm.script(
        &id,
        tool_reply(&[("call-1", "bash", r#"{"command":"make"}"#)]),
    );
    harness.submit(&id, "/loop 3 check the build").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(harness.runtime.abort(&id).await.unwrap());
    harness.settle(&id).await;

    harness.llm.script(&id, text_reply("resumed"));
    harness.llm.script(&id, text_reply("resumed again"));
    harness.submit(&id, "carry on").await;
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 3);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn loop_off_removes_the_status_item_without_a_notice() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 2").await;
    let waiting = harness.runtime.transcript(&id).await.unwrap();
    assert!(
        loop_status(&waiting.items).is_some_and(|status| status.contains("waiting for a prompt"))
    );

    harness.submit(&id, "/loop off").await;
    let transcript = harness.settle(&id).await;

    assert!(loop_status(&transcript.items).is_none());
    assert!(notice_bodies(&transcript.items).is_empty());
    assert!(harness.llm.requests_for(&id).is_empty());
}

#[tokio::test]
async fn an_until_condition_that_passes_halts_the_loop_after_the_run() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    bash_gate_exit(&harness, 0);
    harness.llm.script(&id, text_reply("tests are green"));
    harness
        .submit(&id, "/loop --until 'bun test' fix the failing tests")
        .await;
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert_eq!(harness.tools.calls_to("bash").len(), 1);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| { body == "Loop condition met: `bun test` exited 0. Loop mode disabled." })
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn an_until_condition_that_fails_repeats_until_the_count_runs_out() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    bash_gate_exit(&harness, 1);
    harness.llm.script(&id, text_reply("still red"));
    harness.llm.script(&id, text_reply("still red, again"));
    harness
        .submit(&id, "/loop 2 --until 'bun test' fix the failing tests")
        .await;
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 2);
    assert_eq!(
        harness.tools.calls_to("bash").len(),
        1,
        "the gate runs after the first run only; the count stops the second"
    );
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
}

#[tokio::test]
async fn a_while_condition_that_fails_halts_the_loop() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    bash_gate_exit(&harness, 1);
    harness.llm.script(&id, text_reply("ready file missing"));
    harness
        .submit(&id, "/loop --while 'test -f ready' ship it")
        .await;
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(notice_bodies(&transcript.items).iter().any(|body| {
        body == "Loop condition failed: `test -f ready` exited 1. Loop mode disabled."
    }));
}

#[tokio::test]
async fn a_condition_without_an_exit_status_stops_the_loop_with_an_error() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("working on it"));
    harness.submit(&id, "/loop --until 'bun test' fix it").await;
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body.starts_with("Loop stopped: condition `bun test` did not exit"))
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn loop_off_during_a_condition_check_cancels_it_without_pausing() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    *harness.tools.delay.lock().unwrap() = Some(Duration::from_secs(30));
    harness.llm.script(&id, text_reply("tests are running"));
    harness.submit(&id, "/loop --until 'bun test' fix it").await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    harness.submit(&id, "/loop off").await;
    let transcript = harness.settle(&id).await;

    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(
        loop_status(&transcript.items).is_none(),
        "off must not leave a paused status"
    );
    assert!(notice_bodies(&transcript.items).is_empty());
}

#[tokio::test]
async fn invalid_loop_arguments_are_rejected_without_enabling_the_loop() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    for args in [
        "/loop 0",
        "/loop 10.5",
        "/loop 0m",
        "/loop 1 --bogus",
        "/loop 1 --until 'a' --while 'b'",
    ] {
        let result = harness.runtime.submit(&id, args.to_owned()).await;
        assert!(
            matches!(result, Err(AgentError::InvalidArgument(_))),
            "{args}: {result:?}"
        );
    }
    let transcript = harness.runtime.transcript(&id).await.unwrap();

    assert!(loop_status(&transcript.items).is_none());
    assert!(harness.llm.requests_for(&id).is_empty());
}

#[tokio::test]
async fn a_conversation_without_a_loop_runs_once_and_shows_no_loop_status() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("hi"));
    harness.submit(&id, "hello").await;
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(loop_status(&transcript.items).is_none());
    assert!(notice_bodies(&transcript.items).is_empty());
}

#[tokio::test]
async fn off_during_the_condition_record_lookup_dispatches_no_shell_check() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    bash_gate_exit(&harness, 0);
    harness.llm.script(&id, text_reply("tests are running"));
    harness.submit(&id, "/loop --until 'bun test' fix it").await;
    parked(gate.conversation_held()).await;
    harness.submit(&id, "/loop off").await;
    gate.release_conversation();
    let transcript = harness.settle(&id).await;

    assert!(
        harness.tools.calls_to("bash").is_empty(),
        "a loop stopped during its record lookup must not run the check"
    );
    assert!(loop_status(&transcript.items).is_none());
    assert!(notice_bodies(&transcript.items).is_empty());
}

#[tokio::test]
async fn stop_during_the_repeat_append_prevents_the_repeat() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_user_append(2);
    harness.llm.script(&id, text_reply("first"));
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    assert!(harness.runtime.abort(&id).await.unwrap());
    gate.release_user_append();
    let transcript = harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert_eq!(user_texts(&requests[0]), vec!["bump"]);
    assert!(loop_status(&transcript.items).is_some_and(|status| status.contains("paused")));
    assert!(
        !notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
}

#[tokio::test]
async fn prevented_repeat_keeps_the_remaining_count() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_user_append(2);
    harness.llm.script(&id, text_reply("first"));
    harness.llm.script(&id, text_reply("second"));
    harness.llm.script(&id, text_reply("third"));
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    assert!(harness.runtime.abort(&id).await.unwrap());
    gate.release_user_append();
    harness.settle(&id).await;

    harness.submit(&id, "again").await;
    let transcript = harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        3,
        "the prevented repeat must not spend a run"
    );
    assert_eq!(user_texts(&requests[2]), vec!["bump", "again", "again"]);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn off_during_the_repeat_append_prevents_the_repeat() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_user_append(2);
    harness.llm.script(&id, text_reply("first"));
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    harness.submit(&id, "/loop off").await;
    gate.release_user_append();
    let transcript = harness.settle(&id).await;

    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(loop_status(&transcript.items).is_none());
    assert!(notice_bodies(&transcript.items).is_empty());
}
