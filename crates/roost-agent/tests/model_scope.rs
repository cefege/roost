//! Model selection at turn time: folder defaults over global ones, ordered
//! role chains, failover that never replays visible output, and clearing the
//! session model back to the inherited selection.

#[path = "support/mod.rs"]
mod support;

use std::collections::BTreeMap;

use roost_agent::roles::role_chain;
use roost_agent::{AgentSettings, NewConversation, Role};
use roost_llm::{BlockKind, LlmError, StreamEvent};
use roost_protocol::wire::agent_chat::{FolderModelRoles, ModelRef};
use support::{Harness, ScriptedLlm, text_reply, texts};

const SONNET: &str = "claude-sonnet-5";
const HAIKU: &str = "claude-haiku-4-5";
const OPUS: &str = "claude-opus-5";

async fn unpinned(harness: &Harness, worker_fp: &str, cwd: &str) -> String {
    harness
        .runtime
        .create_conversation(NewConversation {
            worker_fp: worker_fp.into(),
            worker_label: "desktop".into(),
            worker_os: "linux".into(),
            cwd: cwd.into(),
            title: None,
            model: None,
            thinking_level: None,
        })
        .await
        .unwrap()
        .id
}

fn model_ids(harness: &Harness, id: &str) -> Vec<String> {
    harness
        .llm
        .requests_for(id)
        .iter()
        .map(|request| request.model.id.clone())
        .collect()
}

fn sonnet_then_haiku_chain() -> AgentSettings {
    let mut settings = AgentSettings::default();
    settings.model_roles.insert(
        Role::Default,
        format!("anthropic/{SONNET}, anthropic/{HAIKU}"),
    );
    settings
}

#[tokio::test]
async fn folder_default_applies_only_to_its_worker_and_directory() {
    let harness = Harness::new();
    let mut settings = AgentSettings::default();
    settings
        .model_roles
        .insert(Role::Default, format!("anthropic/{SONNET}"));
    settings.folder_model_roles = vec![FolderModelRoles {
        worker_fp: "worker-fp".into(),
        cwd: "/repo".into(),
        model_roles: BTreeMap::from([("default".to_owned(), format!("anthropic/{HAIKU}"))]),
    }];
    harness.runtime.set_settings(settings).await.unwrap();

    let inside = unpinned(&harness, "worker-fp", "/repo").await;
    let other_dir = unpinned(&harness, "worker-fp", "/other").await;
    let other_worker = unpinned(&harness, "worker-two", "/repo").await;
    let views = harness.runtime.conversations().await.unwrap();
    for (id, expected) in [
        (&inside, HAIKU),
        (&other_dir, SONNET),
        (&other_worker, SONNET),
    ] {
        let view = views.iter().find(|view| &view.id == id).unwrap();
        assert_eq!(
            view.model, None,
            "inherited models must not become session pins"
        );
        assert_eq!(
            view.effective_model,
            Some(pin(expected)),
            "the composer must see its actual inherited candidate"
        );
    }
    for id in [&inside, &other_dir, &other_worker] {
        harness.llm.script(id, text_reply("ok"));
    }
    for id in [&inside, &other_dir, &other_worker] {
        harness.submit(id, "hi").await;
        harness.settle(id).await;
    }

    let inside_ids = model_ids(&harness, &inside);
    assert!(!inside_ids.is_empty() && inside_ids.iter().all(|id| id == HAIKU));
    let other_dir_ids = model_ids(&harness, &other_dir);
    assert!(!other_dir_ids.is_empty() && other_dir_ids.iter().all(|id| id == SONNET));
    let other_worker_ids = model_ids(&harness, &other_worker);
    assert!(!other_worker_ids.is_empty() && other_worker_ids.iter().all(|id| id == SONNET));
}

#[tokio::test]
async fn transient_failures_exhaust_retries_then_fail_over_in_chain_order() {
    let harness = Harness::new();
    let settings = sonnet_then_haiku_chain();
    harness
        .runtime
        .set_settings(settings.clone())
        .await
        .unwrap();
    let id = unpinned(&harness, "worker-fp", "/repo").await;

    // The first attempt and the three configured retries all fail on the first candidate.
    for _ in 0..4 {
        harness
            .llm
            .script(&id, Err(LlmError::Network("connection reset".into())));
    }
    harness.llm.script(&id, text_reply("recovered"));
    harness.submit(&id, "hi").await;
    let transcript = harness.settle(&id).await;

    assert!(texts(&transcript).iter().any(|text| text == "recovered"));
    let mut expected = vec![SONNET.to_owned(); 4];
    expected.push(HAIKU.to_owned());
    assert_eq!(model_ids(&harness, &id), expected);
    assert_eq!(harness.runtime.settings().await.unwrap(), settings);
}

#[tokio::test]
async fn no_failover_once_the_response_showed_visible_output() {
    let harness = Harness::new();
    harness
        .runtime
        .set_settings(sonnet_then_haiku_chain())
        .await
        .unwrap();
    let id = unpinned(&harness, "worker-fp", "/repo").await;

    harness.llm.partial_then_fail(
        &id,
        vec![StreamEvent::TextDelta {
            index: 0,
            text: "partial".to_owned(),
        }],
        LlmError::Network("connection reset".into()),
    );
    harness.llm.script(&id, text_reply("same-model retry"));
    harness.submit(&id, "hi").await;
    harness.settle(&id).await;

    let ids = model_ids(&harness, &id);
    assert!(!ids.is_empty() && ids.iter().all(|model| model == SONNET));
}

#[tokio::test]
async fn empty_block_scaffolding_does_not_block_failover() {
    let harness = Harness::new();
    harness
        .runtime
        .set_settings(sonnet_then_haiku_chain())
        .await
        .unwrap();
    let id = unpinned(&harness, "worker-fp", "/repo").await;

    // Each attempt on the first candidate starts a text block, sends an empty
    // delta, then fails before any real output.
    for _ in 0..4 {
        harness.llm.partial_then_fail(
            &id,
            vec![
                StreamEvent::BlockStart {
                    index: 0,
                    kind: BlockKind::Text,
                },
                StreamEvent::TextDelta {
                    index: 0,
                    text: String::new(),
                },
            ],
            LlmError::Network("connection reset".into()),
        );
    }
    harness.llm.script(&id, text_reply("recovered"));
    harness.submit(&id, "hi").await;
    let transcript = harness.settle(&id).await;

    assert!(texts(&transcript).iter().any(|text| text == "recovered"));
    let mut expected = vec![SONNET.to_owned(); 4];
    expected.push(HAIKU.to_owned());
    assert_eq!(model_ids(&harness, &id), expected);
}

#[tokio::test]
async fn an_auth_failure_is_not_replayed_on_the_next_candidate() {
    let harness = Harness::new();
    harness
        .runtime
        .set_settings(sonnet_then_haiku_chain())
        .await
        .unwrap();
    let id = unpinned(&harness, "worker-fp", "/repo").await;

    harness
        .llm
        .script(&id, Err(LlmError::Auth("invalid key".into())));
    harness.llm.script(&id, text_reply("should not run"));
    harness.submit(&id, "hi").await;
    harness.settle(&id).await;

    let ids = model_ids(&harness, &id);
    assert!(!ids.is_empty() && ids.iter().all(|model| model == SONNET));
}

#[tokio::test]
async fn clearing_the_session_model_restores_the_inherited_selection() {
    let harness = Harness::new();
    let pinned = harness.conversation().await;
    assert!(
        harness
            .runtime
            .conversation(&pinned)
            .await
            .unwrap()
            .model
            .is_some()
    );

    harness.runtime.clear_model(&pinned).await.unwrap();
    assert!(
        harness
            .runtime
            .conversation(&pinned)
            .await
            .unwrap()
            .model
            .is_none()
    );

    let fresh = unpinned(&harness, "worker-fp", "/repo").await;
    assert!(
        harness
            .runtime
            .conversation(&fresh)
            .await
            .unwrap()
            .model
            .is_none()
    );
}

#[tokio::test]
async fn role_chain_keeps_configured_order_and_skips_unavailable_models() {
    let llm = ScriptedLlm::new(&["anthropic"]);
    let mut settings = AgentSettings::default();
    settings.model_roles.insert(
        Role::Default,
        format!("openai/gpt-x, anthropic/{SONNET}, anthropic/{HAIKU}"),
    );
    let chain = role_chain(llm.as_ref(), &settings, Role::Default).await;
    let ids: Vec<&str> = chain.iter().map(|model| model.info.id.as_str()).collect();
    assert_eq!(ids, vec![SONNET, HAIKU]);
}

fn pin(model_id: &str) -> ModelRef {
    ModelRef {
        provider: "anthropic".into(),
        model_id: model_id.into(),
    }
}

#[tokio::test]
async fn plan_mode_session_pick_wins_over_planning_and_exit_restores_the_pre_plan_pin() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    let mut settings = AgentSettings::default();
    settings
        .model_roles
        .insert(Role::Plan, format!("anthropic/{OPUS}"));
    harness.runtime.set_settings(settings).await.unwrap();
    harness.runtime.set_model(&id, pin(HAIKU)).await.unwrap();

    harness.submit(&id, "/plan").await;
    harness.settle(&id).await;
    let entered = harness.runtime.conversation(&id).await.unwrap();
    assert_eq!(
        entered.model, None,
        "entering plan mode inherits the Planning role"
    );

    harness.runtime.set_model(&id, pin(SONNET)).await.unwrap();
    harness
        .llm
        .script(&id, text_reply("answered with the session pick"));
    harness.submit(&id, "first question").await;
    harness.settle(&id).await;
    assert_eq!(
        model_ids(&harness, &id).last().map(String::as_str),
        Some(SONNET),
        "a session pick during plan mode outranks the Planning role"
    );

    harness.runtime.clear_model(&id).await.unwrap();
    harness
        .llm
        .script(&id, text_reply("answered with the planning role"));
    harness.submit(&id, "second question").await;
    harness.settle(&id).await;
    assert_eq!(
        model_ids(&harness, &id).last().map(String::as_str),
        Some(OPUS),
        "clearing the pick returns plan turns to the Planning role"
    );

    harness.submit(&id, "/plan").await;
    harness.settle(&id).await;
    let restored = harness.runtime.conversation(&id).await.unwrap();
    assert_eq!(
        restored.model,
        Some(pin(HAIKU)),
        "leaving plan mode restores the model it replaced"
    );
}
