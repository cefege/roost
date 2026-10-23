//! Plan mode's tool restrictions and approval, and the `task` tool's child
//! conversations, their reports, and the nesting limit.

#[path = "support/mod.rs"]
mod support;

#[path = "plan_and_tasks/recovery.rs"]
mod recovery;

use roost_agent::records::{AssistantMetadata, model_ref};
use roost_agent::{AgentSettings, AgentStore, Entry, Role};
use roost_llm::{AssistantBlock, StopReason, Usage};
use roost_protocol::wire::agent_chat::TranscriptItem;
use support::{Harness, text_reply, tool_reply, user_texts};

fn tool_names(request: &roost_llm::ChatRequest) -> Vec<String> {
    request.tools.iter().map(|tool| tool.name.clone()).collect()
}

#[tokio::test]
async fn plan_mode_offers_propose_plan_without_write_tools_and_approval_restores_them() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut settings = AgentSettings::default();
    settings
        .model_roles
        .insert(Role::Plan, "anthropic/claude-opus-5".into());
    harness.runtime.set_settings(settings).await.unwrap();

    harness.llm.script(
        &id,
        tool_reply(&[(
            "call-1",
            "propose_plan",
            r#"{"title":"Add flag","plan":"1. add --version"}"#,
        )]),
    );
    harness.submit(&id, "/plan add a version flag").await;
    let transcript = harness.settle(&id).await;

    let planning = &harness.llm.requests_for(&id)[0];
    let names = tool_names(planning);
    assert!(names.contains(&"propose_plan".to_owned()));
    assert!(!names.contains(&"edit".to_owned()) && !names.contains(&"write".to_owned()));
    assert_eq!(planning.model.id, "claude-opus-5");
    assert_eq!(transcript.mode.as_deref(), Some("plan"));
    let plan_id = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Plan { id, state, .. } if state == "proposed" => Some(id.clone()),
            _ => None,
        })
        .expect("a proposed plan card");
    assert_eq!(
        harness.llm.requests_for(&id).len(),
        1,
        "propose_plan ends the run"
    );

    harness.llm.script(&id, text_reply("executing"));
    harness
        .runtime
        .plan_decide(&id, &plan_id, "approve", "")
        .await
        .unwrap();
    let transcript = harness.settle(&id).await;
    let executing = harness.llm.requests_for(&id).pop().unwrap();
    assert!(tool_names(&executing).contains(&"edit".to_owned()));
    assert_eq!(executing.model.id, support::MODEL);
    assert!(
        user_texts(&executing)
            .last()
            .unwrap()
            .contains("1. add --version")
    );
    assert_eq!(transcript.mode.as_deref(), Some("normal"));
    assert!(
        transcript
            .items
            .iter()
            .any(|item| matches!(item, TranscriptItem::Plan { state, .. } if state == "approved"))
    );
}

#[tokio::test]
async fn task_runs_two_children_and_returns_both_yields() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, tool_reply(&[(
        "call-1",
        "task",
        r#"{"context":"repo tour","tasks":[{"name":"Workers","agent":"scout","task":"find workers"},{"name":"Sessions","agent":"scout","task":"find sessions"}]}"#,
    )]));
    harness.llm.script(&id, text_reply("both found"));
    harness.llm.script(
        support::ANY_SESSION,
        tool_reply(&[("y", "yield", r#"{"result":"report one"}"#)]),
    );
    harness.llm.script(
        support::ANY_SESSION,
        tool_reply(&[("y", "yield", r#"{"result":"report two"}"#)]),
    );
    harness.submit(&id, "map workers and sessions").await;
    let transcript = harness.settle(&id).await;

    let tool = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Tool {
                tool_name,
                output,
                children,
                ..
            } if tool_name == "task" => Some((output.clone(), children.clone())),
            _ => None,
        })
        .expect("task card");
    assert_eq!(tool.1.len(), 2);
    assert!(tool.0.contains("report one") && tool.0.contains("report two"));
    assert!(tool.0.contains("## Workers (scout)") && tool.0.contains("## Sessions (scout)"));
    let child_request = &harness.llm.requests_for(&tool.1[0])[0];
    assert!(
        child_request
            .system
            .iter()
            .any(|block| block.contains("repo tour"))
    );
    assert!(tool_names(child_request).contains(&"yield".to_owned()));
    assert!(
        !tool_names(child_request).contains(&"edit".to_owned()),
        "scouts are read-only"
    );
    let summaries = harness.runtime.conversations().await.unwrap();
    assert!(
        summaries
            .iter()
            .filter(|summary| summary.parent_id.as_deref() == Some(id.as_str()))
            .count()
            == 2
    );
}

#[tokio::test]
async fn a_depth_two_child_is_not_offered_the_task_tool() {
    use roost_agent::toolset::toolset_for;
    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut child = {
        use roost_agent::AgentStore;
        harness.store.conversation(&id).await.unwrap().unwrap()
    };
    child.agent = Some("task".into());
    child.parent_id = Some("parent".into());
    assert!(toolset_for(&child, 1, false).offers("task"));
    assert!(!toolset_for(&child, 2, false).offers("task"));
    assert!(toolset_for(&child, 2, false).offers("yield"));
}

async fn append_journal_entry(harness: &Harness, id: &str, entry: Entry) -> u64 {
    harness
        .store
        .append_entry(id, &entry, &[], None)
        .await
        .unwrap()
}

fn assistant_entry(item_id: &str, text: &str, metadata: Option<AssistantMetadata>) -> Entry {
    Entry::Assistant {
        item_id: item_id.to_owned(),
        blocks: vec![AssistantBlock::Text {
            text: text.to_owned(),
        }],
        usage: Usage::default(),
        model: Some(model_ref(support::PROVIDER, support::MODEL)),
        metadata,
    }
}

#[tokio::test]
async fn recovered_successful_output_finishes_without_repeating_its_primary_call() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let input_seq = append_journal_entry(
        &harness,
        &id,
        Entry::User {
            text: "already answered".into(),
            loop_repeat: false,
            item_id: None,
        },
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        Entry::RunIntent {
            active: true,
            through_seq: input_seq,
            aborted: false,
            yielded: false,
        },
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        assistant_entry(
            "completed-answer",
            "The answer is durable.",
            Some(AssistantMetadata {
                completed: true,
                stop_reason: Some(StopReason::MaxTokens),
                input_through_seq: input_seq,
                continued: false,
                yield_reminders: 0,
                goal_id: None,
            }),
        ),
    )
    .await;

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    harness.settle_runtime(&restarted, &id).await;

    assert!(
        harness.llm.requests_for(&id).is_empty(),
        "the committed answer must not trigger another primary call"
    );
}

#[tokio::test]
async fn late_user_and_advisory_inputs_are_not_marked_consumed_by_prior_output() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let input_seq = append_journal_entry(
        &harness,
        &id,
        Entry::User {
            text: "first request".into(),
            loop_repeat: false,
            item_id: None,
        },
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        Entry::RunIntent {
            active: true,
            through_seq: input_seq,
            aborted: false,
            yielded: false,
        },
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        assistant_entry(
            "first-answer",
            "First answer.",
            Some(AssistantMetadata {
                completed: true,
                stop_reason: Some(StopReason::MaxTokens),
                input_through_seq: input_seq,
                continued: false,
                yield_reminders: 0,
                goal_id: None,
            }),
        ),
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        Entry::User {
            text: "late steer".into(),
            loop_repeat: false,
            item_id: None,
        },
    )
    .await;
    let late_seq = append_journal_entry(
        &harness,
        &id,
        Entry::Advisory {
            item_id: "late-advisory".into(),
            severity: roost_agent::records::AdvisorySeverity::Concern,
            note: "deadline changed".into(),
            delivered: true,
        },
    )
    .await;
    append_journal_entry(
        &harness,
        &id,
        Entry::RunIntent {
            active: true,
            through_seq: late_seq,
            aborted: false,
            yielded: false,
        },
    )
    .await;
    harness.llm.script(&id, text_reply("handled both inputs"));

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    harness.settle_runtime(&restarted, &id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        1,
        "only the late input needs a primary call"
    );
    let request_inputs = user_texts(&requests[0]);
    assert!(request_inputs.iter().any(|text| text == "late steer"));
    assert!(
        request_inputs
            .iter()
            .any(|text| text.contains("deadline changed"))
    );
}

#[test]
fn historical_assistant_rows_default_to_no_completion_metadata() {
    let mut row = serde_json::to_value(assistant_entry("old-row", "old answer", None)).unwrap();
    row.as_object_mut().unwrap().remove("metadata");

    let decoded: Entry = serde_json::from_value(row).unwrap();
    assert!(matches!(decoded, Entry::Assistant { metadata: None, .. }));
}

#[tokio::test]
async fn pending_advisory_card_does_not_restart_a_settled_run() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("finished"));
    harness.submit(&id, "finish this").await;
    harness.settle(&id).await;
    append_journal_entry(
        &harness,
        &id,
        Entry::Advisory {
            item_id: "waiting-review".into(),
            severity: roost_agent::records::AdvisorySeverity::Concern,
            note: "pending review".into(),
            delivered: false,
        },
    )
    .await;

    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 0);
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
}
