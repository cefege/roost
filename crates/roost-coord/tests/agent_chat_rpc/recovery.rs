//! Coordinator boot recovery against a real migrated database and HTTP model.
//! Known tool results survive restart; unresolved side effects are surfaced to
//! the next model request without dispatching those calls again.

use roost_agent::{AgentStore, Entry};
use roost_llm::{AssistantBlock, Usage};
use roost_protocol::wire::agent_chat::{AgentRunState, ModelRef};

use super::{MODEL, add_key, create, settled_transcript, setup, text_sse};

#[tokio::test]
async fn boot_resumes_durable_progress_without_replaying_unknown_tool_effects() {
    let setup = setup("agent-restart-recovery").await;
    add_key(&setup, "sk-ant-recovery-key-0001").await;
    let id = create(&setup).await;
    let store =
        roost_coord::agent::store::CoordAgentStore::new(setup.fixture.core.services.db.clone());
    store
        .append_entry(
            &id,
            &Entry::User {
                text: "Continue the existing migration".into(),
                loop_repeat: false,
            },
            &[],
        )
        .await
        .unwrap();
    store
        .append_entry(
            &id,
            &Entry::Assistant {
                item_id: "before-crash".into(),
                blocks: vec![
                    AssistantBlock::ToolCall {
                        call_id: "known-read".into(),
                        name: "read".into(),
                        args_json: r#"{"path":"migration.sql"}"#.into(),
                    },
                    AssistantBlock::ToolCall {
                        call_id: "unknown-bash".into(),
                        name: "bash".into(),
                        args_json: r#"{"command":"apply-migration"}"#.into(),
                    },
                ],
                usage: Usage::default(),
                model: Some(ModelRef {
                    provider: "anthropic".into(),
                    model_id: MODEL.into(),
                }),
                metadata: None,
            },
            &[],
        )
        .await
        .unwrap();
    store
        .append_entry(
            &id,
            &Entry::ToolResult {
                call_id: "known-read".into(),
                tool_name: "read".into(),
                text: "ALTER TABLE projects ADD COLUMN progress TEXT".into(),
                is_error: false,
                details_json: "{}".into(),
            },
            &[],
        )
        .await
        .unwrap();
    let mut record = store.conversation(&id).await.unwrap().unwrap();
    record.run_state = AgentRunState::Running;
    store.save_conversation(&record).await.unwrap();
    setup
        .fake
        .replies
        .lock()
        .push(text_sse("Recovered the existing migration."));

    setup.fixture.core.services.agent.recover().await;
    let transcript = settled_transcript(&setup, &id).await;
    assert_eq!(transcript["run_state"], "idle", "{transcript}");
    let history = {
        let requests = setup.fake.seen_bodies.lock();
        let restored = requests
            .iter()
            .find(|request| {
                request["messages"]
                    .to_string()
                    .contains("Continue the existing migration")
            })
            .expect("a model request resumed the saved user submission");
        restored["messages"].to_string()
    };
    assert!(
        history.contains("ALTER TABLE projects ADD COLUMN progress TEXT"),
        "{history}"
    );
    assert!(
        history.contains("unknown-bash") && history.contains("side effects are unknown"),
        "{history}"
    );
    assert!(
        setup
            .worker
            .calls
            .lock()
            .iter()
            .all(|call| call.tool != "bash")
    );
    let entries = store.entries(&id).await.unwrap();
    assert!(entries.iter().any(|(_, entry)| matches!(entry,
        Entry::ToolResult { call_id, is_error: true, .. } if call_id == "unknown-bash")));
    let count_before = setup
        .fake
        .seen_bodies
        .lock()
        .iter()
        .filter(|request| {
            request["messages"]
                .to_string()
                .contains("Continue the existing migration")
        })
        .count();
    setup.fixture.core.services.agent.recover().await;
    let count_after = setup
        .fake
        .seen_bodies
        .lock()
        .iter()
        .filter(|request| {
            request["messages"]
                .to_string()
                .contains("Continue the existing migration")
        })
        .count();
    assert_eq!(
        count_after, count_before,
        "completed work must not run again"
    );
}
