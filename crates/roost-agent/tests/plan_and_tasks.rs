//! Plan mode's tool restrictions and approval, and the `task` tool's child
//! conversations, their reports, and the nesting limit.

#[path = "support/mod.rs"]
mod support;

use roost_agent::{AgentSettings, Role};
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
