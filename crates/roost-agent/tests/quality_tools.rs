//! Coordinator harness tools, interactive answers, side questions, and durable advisor state.
//! Scripted provider and worker seams exercise their visible conversation effects.
//! Recovery assertions preserve completed output and durable advisor state.

#[path = "support/mod.rs"]
mod support;

#[path = "quality_tools/recovery.rs"]
mod recovery;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use roost_agent::{
    AgentRuntime, AgentSettings, AgentStore, ChatSink, Entry, Llm, Role, RuntimeConfig,
    ToolExecutor,
};
use roost_protocol::wire::agent_chat::{
    AgentRunState, FolderModelRoles, TodoStatus, TranscriptItem,
};
use support::{Harness, text_reply, tool_reply, user_texts};

#[tokio::test]
async fn todo_operations_auto_advance_and_update_the_single_todo_card() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(
        &id,
        tool_reply(&[(
            "init",
            "todo",
            r#"{"op":"init","list":[{"phase":"Build","items":["write tests","implement tool"]}]}"#,
        )]),
    );
    harness.llm.script(
        &id,
        tool_reply(&[("start", "todo", r#"{"op":"start","task":"implement tool"}"#)]),
    );
    harness.llm.script(
        &id,
        tool_reply(&[("done", "todo", r#"{"op":"done","task":"implement tool"}"#)]),
    );
    harness
        .llm
        .script(&id, text_reply("The first task remains active."));
    harness.submit(&id, "Implement both tasks").await;
    let transcript = harness.settle(&id).await;

    let cards = transcript
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::Todo { phases, .. } => Some(phases),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0][0].tasks[0].status, TodoStatus::InProgress);
    assert_eq!(cards[0][0].tasks[1].status, TodoStatus::Completed);
    assert_eq!(harness.llm.requests_for(&id).len(), 4);
}

#[tokio::test]
async fn ask_ends_its_run_and_answer_restarts_with_answers_context() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.llm.script(&id, tool_reply(&[("ask", "ask", r#"{"questions":[{"question":"Which API?","options":[{"label":"REST"},{"label":"RPC"}]}]}"#)]));
    harness.submit(&id, "Choose the API").await;
    let transcript = harness.settle(&id).await;
    let (item_id, question_id) = transcript
        .items
        .iter()
        .find_map(|item| match item {
            TranscriptItem::Ask {
                id,
                questions,
                answers: None,
            } => Some((id.clone(), questions[0].id.clone())),
            _ => None,
        })
        .expect("ask card remains unanswered");
    assert_eq!(
        harness.llm.requests_for(&id).len(),
        1,
        "ask ends the model run"
    );

    harness.llm.script(&id, text_reply("Proceeding with REST."));
    harness
        .runtime
        .answer(
            &id,
            &item_id,
            BTreeMap::from([(question_id, vec!["REST".into()])]),
        )
        .await
        .unwrap();
    let transcript = harness.settle(&id).await;
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Ask { id, answers: Some(_), .. } if id == &item_id)));
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    assert!(
        user_texts(&requests[1])
            .iter()
            .any(|text| text.contains("<answers>") && text.contains("REST"))
    );
}

#[tokio::test]
async fn btw_is_toolless_and_private_while_a_run_is_active_and_rename_updates_title() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let conversation = harness.runtime.conversation(&id).await.unwrap();
    harness
        .runtime
        .set_settings(AgentSettings {
            model_roles: BTreeMap::from([(Role::Tiny, "anthropic/claude-opus-5".into())]),
            folder_model_roles: vec![FolderModelRoles {
                worker_fp: conversation.worker_fp,
                cwd: conversation.cwd,
                model_roles: BTreeMap::from([("tiny".into(), "anthropic/claude-haiku-4-5".into())]),
            }],
            ..AgentSettings::default()
        })
        .await
        .unwrap();
    *harness.tools.delay.lock().unwrap() = Some(Duration::from_millis(120));
    harness.llm.script(
        &id,
        tool_reply(&[("work", "bash", r#"{"command":"sleep 0.1"}"#)]),
    );
    harness.llm.script(&id, text_reply("Work complete."));
    harness.llm.script(
        &format!("{id}-btw"),
        text_reply("The side answer is private."),
    );
    harness.submit(&id, "Do some work").await;
    for _ in 0..100 {
        if !harness.tools.calls_to("bash").is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        harness.runtime.transcript(&id).await.unwrap().run_state,
        AgentRunState::Running
    );
    harness
        .submit(&id, "/btw what is the current design?")
        .await;
    let transcript = harness.settle(&id).await;
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Btw { question, answer: Some(answer), error: None, .. }
            if question == "what is the current design?" && answer == "The side answer is private.")));
    let side = harness.llm.requests_for(&format!("{id}-btw"));
    assert_eq!(side.len(), 1);
    assert_eq!(side[0].model.id, "claude-haiku-4-5");
    assert_ne!(
        side[0].thinking, "auto",
        "provider requests must resolve automatic effort"
    );
    assert!(
        !side[0].messages.iter().any(|message| match message {
            roost_llm::Message::ToolResult { .. } => true,
            roost_llm::Message::Assistant { blocks } => blocks
                .iter()
                .any(|block| { matches!(block, roost_llm::AssistantBlock::ToolCall { .. }) }),
            roost_llm::Message::User { .. } => false,
        }),
        "a toolless side question must not replay executable tool history"
    );
    assert!(side[0].tools.is_empty());
    let normal = harness.llm.requests_for(&id);
    assert!(normal.iter().all(|request| {
        !user_texts(request)
            .iter()
            .any(|text| text.contains("what is the current design?"))
    }));

    harness.submit(&id, "/rename A clear title").await;
    assert_eq!(
        harness.runtime.conversation(&id).await.unwrap().title,
        "A clear title"
    );
}

#[tokio::test]
async fn advisor_cursor_and_counters_survive_a_new_runtime() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness
        .runtime
        .set_settings(AgentSettings {
            advisor_enabled: true,
            ..AgentSettings::default()
        })
        .await
        .unwrap();
    harness.llm.script(&id, text_reply("First result."));
    harness
        .llm
        .script(&format!("{id}-advisor"), text_reply("Review one."));
    harness.submit(&id, "First request").await;
    harness.settle(&id).await;
    for _ in 0..100 {
        if harness.store.advisor_state(&id).await.unwrap().is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let persisted = harness
        .store
        .advisor_state(&id)
        .await
        .unwrap()
        .expect("advisor state persisted");
    let saved: serde_json::Value = serde_json::from_str(&persisted).unwrap();
    let saved_cursor = saved["cursor"].as_u64().unwrap();
    assert!(saved_cursor > 0);

    let runtime = AgentRuntime::new(
        harness.store.clone() as Arc<dyn AgentStore>,
        harness.tools.clone() as Arc<dyn ToolExecutor>,
        harness.sink.clone() as Arc<dyn ChatSink>,
        harness.llm.clone() as Arc<dyn Llm>,
        RuntimeConfig {
            retry_backoff: vec![Duration::from_millis(1); 3],
            advisor_backoff: vec![Duration::from_millis(1); 3],
            find_timeout: Duration::from_secs(5),
        },
    );
    harness.llm.script(&id, text_reply("Second result."));
    harness
        .llm
        .script(&format!("{id}-advisor"), text_reply("Review two."));
    runtime.submit(&id, "Second request".into()).await.unwrap();
    for _ in 0..200 {
        let state = harness.store.advisor_state(&id).await.unwrap();
        if state
            .as_ref()
            .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
            .is_some_and(|value| value["reviews"].as_u64() == Some(2))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let persisted = harness
        .store
        .advisor_state(&id)
        .await
        .unwrap()
        .expect("updated advisor state");
    let state: serde_json::Value = serde_json::from_str(&persisted).unwrap();
    assert_eq!(
        state["reviews"].as_u64(),
        Some(2),
        "loaded review counter did not reset"
    );
    assert!(state["cursor"].as_u64().unwrap() > saved_cursor);
    assert!(
        harness
            .store
            .entries(&id)
            .await
            .unwrap()
            .iter()
            .any(|(_, entry)| matches!(entry, Entry::Assistant { .. }))
    );
}

#[tokio::test]
async fn todo_and_ask_are_offered_only_to_the_intended_agent_kinds() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let record = harness.store.conversation(&id).await.unwrap().unwrap();
    let top_level = roost_agent::toolset::toolset_for(&record, 0, false);
    assert!(top_level.offers("todo"));
    assert!(top_level.offers("ask"));
    assert!(top_level.offers("web_search"));

    let mut child = record.clone();
    child.parent_id = Some(id);
    child.agent = Some("task".into());
    let task = roost_agent::toolset::toolset_for(&child, 1, false);
    assert!(task.offers("todo"));
    assert!(!task.offers("ask"));
    child.agent = Some("reviewer".into());
    let reviewer = roost_agent::toolset::toolset_for(&child, 1, false);
    assert!(reviewer.offers("todo"));
    assert!(!reviewer.offers("ask"));
    child.agent = Some("scout".into());
    let scout = roost_agent::toolset::toolset_for(&child, 1, false);
    assert!(!scout.offers("todo"));
    assert!(scout.offers("fetch"));
    assert!(scout.offers("web_search"));
    assert!(!scout.offers("ask"));
}

#[tokio::test]
async fn every_agent_kind_and_mode_offers_each_tool_name_once() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let record = harness.store.conversation(&id).await.unwrap().unwrap();
    for agent in [None, Some("task"), Some("reviewer"), Some("scout")] {
        for mode in [roost_agent::Mode::Normal, roost_agent::Mode::Plan] {
            for find_available in [false, true] {
                let mut variant = record.clone();
                variant.agent = agent.map(str::to_owned);
                variant.mode = mode;
                let toolset = roost_agent::toolset::toolset_for(&variant, 1, find_available);
                let mut names: Vec<&str> = toolset
                    .specs
                    .iter()
                    .map(|spec| spec.name.as_str())
                    .collect();
                let offered = names.len();
                names.sort_unstable();
                names.dedup();
                assert_eq!(names.len(), offered, "{agent:?} {mode:?}: {names:?}");
            }
        }
    }
}
