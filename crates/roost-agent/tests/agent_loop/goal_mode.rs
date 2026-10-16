//! Goal lifecycle, budget accounting, and restart policy through the full runtime.
//! Scripted provider responses distinguish explicit completion from prose.
//! The shared fold is checked against the durable journal rather than UI wording.

use roost_agent::{AgentStore, Entry};
use roost_llm::{AssistantBlock, StreamEvent, Usage};
use roost_protocol::wire::agent_chat::{
    GoalState, GoalStatus, TodoPhase, TodoStatus, TodoTask, TranscriptItem,
};
use tokio_util::sync::CancellationToken;

use super::support::{Harness, text_reply, tool_reply, user_texts};

pub(super) fn goal(id: &str, status: GoalStatus, waiting_reason: Option<&str>) -> GoalState {
    GoalState {
        id: id.into(),
        objective: "Ship the feature".into(),
        status,
        token_budget: None,
        tokens_used: 0,
        time_used_seconds: 0,
        created_ms: 1,
        updated_ms: 1,
        report: None,
        waiting_reason: waiting_reason.map(str::to_owned),
    }
}

pub(super) async fn persist_goal(harness: &Harness, conversation_id: &str, state: GoalState) {
    harness
        .store
        .append_entry(
            conversation_id,
            &Entry::Goal { state },
            &[] as &[&CancellationToken],
            None,
        )
        .await
        .unwrap();
}

pub(super) fn projected_goal(
    transcript: &roost_protocol::wire::agent_chat::Transcript,
) -> GoalState {
    transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Goal { state, .. } => Some(state.clone()),
            _ => None,
        })
        .expect("goal is projected")
}

#[tokio::test]
async fn prose_does_not_complete_a_goal_but_the_explicit_goal_tool_does() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .llm
        .script(&id, text_reply("Everything is finished."));
    harness.submit(&id, "/goal Ship the feature").await;
    let transcript = harness.settle(&id).await;
    let first_state = projected_goal(&transcript);
    assert_eq!(first_state.status, GoalStatus::Active);
    assert!(
        first_state
            .waiting_reason
            .as_deref()
            .unwrap()
            .contains("No tool activity")
    );

    harness.submit(&id, "/goal pause").await;
    harness.llm.script(
        &id,
        tool_reply(&[(
            "goal-done",
            "goal",
            r#"{"op":"complete","objective":"Verified release"}"#,
        )]),
    );
    harness
        .llm
        .script(&id, text_reply("The requested evidence is recorded."));
    harness.submit(&id, "/goal resume").await;
    let transcript = harness.settle(&id).await;
    let state = projected_goal(&transcript);
    assert_eq!(state.status, GoalStatus::Complete);
    assert_eq!(state.report.as_deref(), Some("Verified release"));
}

#[tokio::test]
async fn budget_counts_input_output_and_cache_write_once_but_not_cache_read() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut response = text_reply("Still working.").unwrap();
    response[3] = StreamEvent::Usage(Usage {
        input: 100,
        output: 10,
        cache_write: 7,
        cache_read: 900,
    });
    harness.llm.script(&id, Ok(response));
    harness.submit(&id, "/goal Ship the feature").await;
    let first = harness.settle(&id).await;
    assert_eq!(projected_goal(&first).tokens_used, 117);

    harness.llm.script(&id, text_reply("Budget wrap-up."));
    harness.submit(&id, "/goal budget 116").await;
    let limited = projected_goal(&harness.settle(&id).await);
    assert_eq!(limited.tokens_used, 237);
    assert_eq!(limited.status, GoalStatus::BudgetLimited);
    assert_eq!(limited.token_budget, Some(116));

    harness
        .llm
        .script(&id, text_reply("Continuing after the budget was cleared."));
    harness.submit(&id, "/goal budget off").await;
    let resumed = harness.settle(&id).await;
    let state = projected_goal(&resumed);
    assert_eq!(state.status, GoalStatus::Active);
    assert_eq!(state.token_budget, None);
    assert_eq!(state.tokens_used, 357);
    assert_eq!(harness.llm.requests_for(&id).len(), 3);
}

#[tokio::test]
async fn budget_exhaustion_schedules_one_hidden_wrap_up_turn() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut state = goal("budget", GoalStatus::Active, None);
    state.token_budget = Some(100);
    persist_goal(&harness, &id, state).await;
    harness
        .store
        .append_entry(
            &id,
            &Entry::Assistant {
                item_id: "over-budget".into(),
                blocks: vec![AssistantBlock::Text {
                    text: "Work completed.".into(),
                }],
                usage: Usage {
                    input: 100,
                    output: 20,
                    cache_read: 0,
                    cache_write: 0,
                },
                model: None,
                metadata: Some(roost_agent::records::AssistantMetadata {
                    completed: true,
                    stop_reason: Some(roost_llm::StopReason::EndTurn),
                    input_through_seq: 0,
                    continued: false,
                    yield_reminders: 0,
                    goal_id: Some("budget".into()),
                }),
            },
            &[] as &[&CancellationToken],
            None,
        )
        .await
        .unwrap();
    harness
        .llm
        .script(&id, text_reply("Progress report and next step."));
    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let transcript = harness.settle_runtime(&restarted, &id).await;
    assert_eq!(
        projected_goal(&transcript).status,
        GoalStatus::BudgetLimited
    );
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 1);
    assert!(format!("{:?}", requests[0].messages).contains("<goal-budget-wrap-up>"));
}

#[tokio::test]
async fn recovery_resumes_only_an_active_goal_without_a_waiting_latch() {
    let harness = Harness::new();
    let active_id = harness.conversation().await;
    persist_goal(
        &harness,
        &active_id,
        goal("active", GoalStatus::Active, None),
    )
    .await;
    harness
        .llm
        .script(&active_id, text_reply("I will make progress."));
    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    harness.settle_runtime(&restarted, &active_id).await;
    assert_eq!(harness.llm.requests_for(&active_id).len(), 1);

    for (status, waiting) in [
        (GoalStatus::Paused, None),
        (GoalStatus::Complete, None),
        (GoalStatus::Dropped, None),
        (GoalStatus::Active, Some("Waiting for the user")),
    ] {
        let conversation_id = harness.conversation().await;
        persist_goal(
            &harness,
            &conversation_id,
            goal("inactive", status, waiting),
        )
        .await;
        let restarted = harness.restart_runtime();
        restarted.recover_interrupted_runs().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(900)).await;
        assert!(harness.llm.requests_for(&conversation_id).is_empty());
    }
}

#[tokio::test]
async fn pause_and_drop_are_durable_and_keep_the_last_goal_report() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .llm
        .script(&id, text_reply("The goal remains open."));
    harness.submit(&id, "/goal Ship the feature").await;
    harness.settle(&id).await;
    harness.submit(&id, "/goal pause").await;
    let paused = projected_goal(&harness.runtime.transcript(&id).await.unwrap());
    assert_eq!(paused.status, GoalStatus::Paused);
    harness.submit(&id, "/goal drop").await;
    let dropped = projected_goal(&harness.runtime.transcript(&id).await.unwrap());
    assert_eq!(dropped.status, GoalStatus::Dropped);
    assert_eq!(dropped.objective, "Ship the feature");
}

#[tokio::test]
async fn hidden_goal_continuation_yields_to_new_user_input() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .llm
        .script(&id, text_reply("I need to inspect the project."));
    harness
        .llm
        .script(&id, text_reply("I will address that request first."));
    harness.submit(&id, "/goal Ship the feature").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    harness
        .submit(&id, "Please prioritize the release notes.")
        .await;
    let transcript = harness.settle(&id).await;
    assert_eq!(
        transcript
            .items
            .iter()
            .filter(|item| matches!(item, TranscriptItem::User { text, .. } if text == "Please prioritize the release notes."))
            .count(),
        1
    );
    let requests = harness.llm.requests_for(&id);
    assert!(requests.iter().any(|request| {
        user_texts(request)
            .iter()
            .any(|text| text.contains("Please prioritize the release notes."))
    }));
    assert!(
        !requests
            .iter()
            .any(|request| format!("{:?}", request.messages).contains("<goal-continuation>"))
    );
}

#[tokio::test]
async fn a_goal_tool_completion_is_not_repeated_as_a_hidden_continuation() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(
        &id,
        tool_reply(&[(
            "complete-goal",
            "goal",
            r#"{"op":"complete","objective":"Tests passed"}"#,
        )]),
    );
    harness.llm.script(&id, text_reply("The tests passed."));
    harness.submit(&id, "/goal Ship the feature").await;
    let transcript = harness.settle(&id).await;
    assert_eq!(projected_goal(&transcript).status, GoalStatus::Complete);
    assert_eq!(harness.llm.requests_for(&id).len(), 2);
}

#[tokio::test]
async fn blocked_only_todos_wait_without_completing_or_resuming_on_restart() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    persist_goal(&harness, &id, goal("blocked", GoalStatus::Active, None)).await;
    harness
        .store
        .append_entry(
            &id,
            &Entry::Todo {
                phases: vec![TodoPhase {
                    name: "Release".into(),
                    tasks: vec![TodoTask {
                        content: "Wait for deployment access".into(),
                        status: TodoStatus::Blocked,
                        blocker: Some("Operator must grant access".into()),
                    }],
                }],
            },
            &[] as &[&CancellationToken],
            None,
        )
        .await
        .unwrap();
    harness
        .llm
        .script(&id, text_reply("I cannot proceed without access."));
    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let transcript = harness.settle_runtime(&restarted, &id).await;
    let state = projected_goal(&transcript);
    assert_eq!(state.status, GoalStatus::Active);
    assert!(state.waiting_reason.is_some());
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    let later_restart = harness.restart_runtime();
    later_restart.recover_interrupted_runs().await.unwrap();
    assert_eq!(
        later_restart.transcript(&id).await.unwrap().run_state,
        roost_protocol::wire::agent_chat::AgentRunState::Idle
    );
}

#[tokio::test]
async fn goal_objectives_that_begin_with_a_keyword_are_kept_whole() {
    let harness = Harness::new();
    for objective in [
        "drop support for the legacy API",
        "show the logs in the dashboard",
        "pause after the migration",
        "resume the sync job",
        "budget 50k tokens",
    ] {
        let id = harness.conversation().await;
        harness.llm.script(&id, text_reply("Working on it."));
        harness.submit(&id, &format!("/goal {objective}")).await;
        let transcript = harness.settle(&id).await;
        let state = projected_goal(&transcript);
        assert_eq!(state.objective, objective);
        assert_eq!(state.status, GoalStatus::Active);
    }
}
