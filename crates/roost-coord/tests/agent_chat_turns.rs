//! Agent chat answer and fork RPC boundary behavior.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use agent_fixture::{AgentFixture, WORKER_A};
use connectrpc::ErrorCode;
use roost_agent::{AgentStore, Entry};
use roost_coord::agent::rpc_chat::handle_agent_chat_create;
use roost_coord::agent::rpc_turns::{handle_agent_chat_answer, handle_agent_chat_fork};
use roost_proto as proto;

#[tokio::test]
async fn invalid_answer_json_is_rejected_as_invalid_argument() {
    let fixture = AgentFixture::new("agent-chat-answer-invalid").await;
    let error = handle_agent_chat_answer(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatAnswerRequest {
            conversation_id: "missing".into(),
            item_id: "ask-1".into(),
            answers_json: "{".into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("invalid answers JSON accepted");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn fork_returns_new_id_and_copies_transcript() {
    let fixture = AgentFixture::new("agent-chat-fork").await;
    let created = handle_agent_chat_create(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatCreateRequest {
            worker_fp: WORKER_A.into(),
            cwd: "/repo".into(),
            model_provider: "anthropic".into(),
            model_id: "claude-sonnet-5".into(),
            ..Default::default()
        },
    )
    .await
    .expect("conversation created")
    .body;
    let store = roost_coord::agent::store::CoordAgentStore::new(fixture.core.services.db.clone());
    store
        .append_entry(
            &created.id,
            &Entry::User {
                text: "copied".into(),
            },
        )
        .await
        .expect("append source transcript");
    let fork = handle_agent_chat_fork(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatForkRequest {
            conversation_id: created.id.clone(),
            ..Default::default()
        },
    )
    .await
    .expect("fork conversation")
    .body;
    assert_ne!(fork.id, created.id);
    let entries = store.entries(&fork.id).await.expect("fork transcript");
    assert!(
        entries
            .iter()
            .any(|(_, entry)| matches!(entry, Entry::User { text } if text == "copied"))
    );
}
