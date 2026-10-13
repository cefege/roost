//! Consumer-visible goal continuation and accounting boundaries.
//! Scripted provider/tool seams exercise the full runtime, journal, and fold.
//! These cases prevent snapshots and inactive goals from changing execution.

use roost_protocol::wire::agent_chat::{GoalStatus, TranscriptItem};

use super::goal_mode::{goal, persist_goal, projected_goal};
use super::support::{Harness, text_reply, tool_reply};

#[tokio::test]
async fn tool_progress_continues_after_usage_snapshots_until_explicit_completion() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.tools.answer("read", "Verified fixture.");
    harness.llm.script(
        &id,
        tool_reply(&[("inspect", "read", r#"{"path":"fixture.txt"}"#)]),
    );
    harness.llm.script(
        &id,
        text_reply("Inspection succeeded; verification remains."),
    );
    harness.llm.script(
        &id,
        tool_reply(&[(
            "done",
            "goal",
            r#"{"op":"complete","objective":"Fixture verified"}"#,
        )]),
    );
    harness.llm.script(&id, text_reply("Fixture verified."));
    harness.submit(&id, "/goal Verify the fixture").await;
    let transcript = harness.settle(&id).await;
    let state = projected_goal(&transcript);
    assert_eq!(state.status, GoalStatus::Complete);
    assert_eq!(state.report.as_deref(), Some("Fixture verified"));
    assert_eq!(harness.llm.requests_for(&id).len(), 4);
    assert_eq!(
        transcript
            .items
            .iter()
            .filter(|item| matches!(item, TranscriptItem::User { .. }))
            .count(),
        1
    );
    assert_eq!(harness.tools.calls_to("read").len(), 1);
}

#[tokio::test]
async fn a_live_turn_reaching_the_budget_gets_exactly_one_hidden_wrap_up() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut state = goal("live-budget", GoalStatus::Active, None);
    state.token_budget = Some(100);
    persist_goal(&harness, &id, state).await;
    harness
        .llm
        .script(&id, text_reply("The budget is now spent."));
    harness.llm.script(
        &id,
        text_reply("Verified progress; remaining work needs more budget."),
    );
    harness.submit(&id, "Perform the work").await;
    let transcript = harness.settle(&id).await;
    let state = projected_goal(&transcript);
    assert_eq!(state.status, GoalStatus::BudgetLimited);
    assert_eq!(state.tokens_used, 240);
    assert_eq!(harness.llm.requests_for(&id).len(), 2);
    assert_eq!(
        transcript
            .items
            .iter()
            .filter(|item| matches!(item, TranscriptItem::User { .. }))
            .count(),
        1
    );
    assert!(
        format!("{:?}", harness.llm.requests_for(&id)[1].messages)
            .contains("<goal-budget-wrap-up>")
    );
}

#[tokio::test]
async fn unrelated_turns_do_not_charge_paused_dropped_or_completed_goals() {
    let harness = Harness::new();
    for status in [
        GoalStatus::Paused,
        GoalStatus::Dropped,
        GoalStatus::Complete,
    ] {
        let id = harness.conversation().await;
        let mut state = goal("inactive-accounting", status, None);
        state.tokens_used = 42;
        state.time_used_seconds = 7;
        persist_goal(&harness, &id, state).await;
        harness.llm.script(&id, text_reply("An unrelated answer."));
        harness.submit(&id, "Answer an unrelated question").await;
        let transcript = harness.settle(&id).await;
        let state = projected_goal(&transcript);
        assert_eq!(state.status, status);
        assert_eq!(state.tokens_used, 42);
        assert_eq!(state.time_used_seconds, 7);
    }
}

#[tokio::test]
async fn clearing_the_budget_during_wrap_up_rearms_after_that_request() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut state = goal("in-flight-budget", GoalStatus::Active, None);
    state.token_budget = Some(100);
    persist_goal(&harness, &id, state).await;
    harness.llm.script(&id, text_reply("Budget spent."));
    harness.llm.script(&id, text_reply("Wrapping up."));
    harness.llm.script(
        &id,
        tool_reply(&[("complete", "goal", r#"{"op":"complete"}"#)]),
    );
    harness.llm.script(&id, text_reply("Complete."));
    let gate = harness.llm.hold_stream(&id, 2);
    harness.submit(&id, "Perform the work").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    harness.submit(&id, "/goal budget off").await;
    gate.released.notify_one();
    let transcript = harness.settle(&id).await;
    let state = projected_goal(&transcript);
    assert_eq!(state.status, GoalStatus::Complete);
    assert_eq!(state.token_budget, None);
    assert_eq!(state.tokens_used, 360);
    assert_eq!(harness.llm.requests_for(&id).len(), 4);
    assert_eq!(
        transcript
            .items
            .iter()
            .filter(|item| matches!(item, TranscriptItem::User { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn resume_during_an_in_flight_request_survives_no_progress_finalization() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    persist_goal(
        &harness,
        &id,
        goal("in-flight-resume", GoalStatus::Active, None),
    )
    .await;
    harness
        .llm
        .script(&id, text_reply("The accepted request finished."));
    harness.llm.script(
        &id,
        tool_reply(&[("complete", "goal", r#"{"op":"complete"}"#)]),
    );
    harness.llm.script(&id, text_reply("Complete."));
    let gate = harness.llm.hold_stream(&id, 1);
    harness.submit(&id, "Perform the work").await;
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered.notified())
        .await
        .unwrap();
    harness.submit(&id, "/goal pause").await;
    harness.submit(&id, "/goal resume").await;
    gate.released.notify_one();
    let transcript = harness.settle(&id).await;
    assert_eq!(projected_goal(&transcript).status, GoalStatus::Complete);
    assert_eq!(harness.llm.requests_for(&id).len(), 3);
    assert_eq!(
        transcript
            .items
            .iter()
            .filter(|item| matches!(item, TranscriptItem::User { .. }))
            .count(),
        1
    );
}
