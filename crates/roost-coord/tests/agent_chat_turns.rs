//! Agent chat answer and fork RPC boundary behavior.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod agent_fixture;
mod db_support;

use agent_fixture::{AgentFixture, WORKER_A};
use connectrpc::ErrorCode;
use roost_agent::records::PlanState;
use roost_agent::{AgentStore, Entry};
use roost_coord::agent::rpc_chat::handle_agent_chat_create;
use roost_coord::agent::rpc_turns::{
    handle_agent_chat_answer, handle_agent_chat_fork, handle_agent_chat_rewind,
};
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
            request_id: "answer-1".into(),
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
                attachments: Vec::new(),
                loop_repeat: false,
                item_id: None,
            },
            &[],
            None,
        )
        .await
        .expect("append source transcript");
    let fork_request = proto::AgentChatForkRequest {
        conversation_id: created.id.clone(),
        request_id: "fork-1".into(),
        ..Default::default()
    };
    let fork = handle_agent_chat_fork(&fixture.core, &fixture.caller, fork_request.clone())
        .await
        .expect("fork conversation")
        .body;
    assert_ne!(fork.id, created.id);
    let entries = store.entries(&fork.id).await.expect("fork transcript");
    assert!(
        entries
            .iter()
            .any(|(_, entry)| matches!(entry, Entry::User { text, .. } if text == "copied"))
    );

    let retried = handle_agent_chat_fork(&fixture.core, &fixture.caller, fork_request)
        .await
        .expect("retried fork")
        .body;
    assert_eq!(
        retried.id, fork.id,
        "a retried request returns the first fork"
    );
    let forks = store
        .conversations()
        .await
        .unwrap()
        .into_iter()
        .filter(|record| record.title.ends_with("(fork)"))
        .count();
    assert_eq!(forks, 1, "a retried request does not fork twice");
}

#[tokio::test]
async fn a_command_without_a_request_id_is_rejected() {
    let fixture = AgentFixture::new("agent-chat-fork-no-request").await;
    let error = handle_agent_chat_fork(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatForkRequest {
            conversation_id: "any".into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("missing request id accepted");
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn rewind_is_refused_while_a_plan_is_pending() {
    let fixture = AgentFixture::new("agent-chat-rewind-pending-plan").await;
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
    .unwrap()
    .body;
    let store = roost_coord::agent::store::CoordAgentStore::new(fixture.core.services.db.clone());
    for entry in [
        Entry::User {
            text: "do not rewind".into(),
            attachments: Vec::new(),
            loop_repeat: false,
            item_id: Some("rewind-me".into()),
        },
        Entry::PlanProposal {
            item_id: "plan".into(),
            title: "Plan".into(),
            content: "1. decide first".into(),
            state: PlanState::Proposed,
        },
    ] {
        store
            .append_entry(&created.id, &entry, &[], None)
            .await
            .unwrap();
    }
    let error = handle_agent_chat_rewind(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatRewindRequest {
            conversation_id: created.id.clone(),
            item_id: "rewind-me".into(),
            request_id: "rewind-pending-plan".into(),
            ..Default::default()
        },
    )
    .await
    .expect_err("pending plan permits rewind");
    assert_eq!(error.code, ErrorCode::FailedPrecondition);
    assert!(
        !store
            .entries(&created.id)
            .await
            .unwrap()
            .iter()
            .any(|(_, entry)| { matches!(entry, Entry::Rewind { .. }) })
    );
}
