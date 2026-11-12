//! Archived native rows remain unreadable and immutable through active chat paths.
//! Exercises RPC resolution, age-based pruning and cross-replica notice relay.
//! Uses the migrated coordinator database without a worker or provider process.

mod agent_fixture;
mod db_support;

use std::sync::Arc;
use std::time::Duration;

use agent_fixture::{AgentFixture, WORKER_A};
use connectrpc::{ConnectError, ErrorCode};
use roost_coord::agent::{chat_events, rpc_chat::*, rpc_chat_transcript::*};
use roost_coord::cluster::notify::CHAT_CHANNEL;
use roost_proto as proto;

async fn seed(fixture: &AgentFixture, id: &str, backend: &str) {
    sqlx::query("INSERT INTO agent_conversations (id,title,worker_fp,worker_label,worker_os,cwd,thinking_level,mode,run_state,created_ms,updated_ms,backend) VALUES ($1,'saved',$2,'worker','linux','/work','off','normal','idle',1000,1000,$3)")
        .bind(id).bind(WORKER_A).bind(backend)
        .execute(fixture.database().pool()).await.unwrap();
    for cursor in 1_i64..=4 {
        let created = if cursor == 3 { i64::MAX } else { 0 };
        sqlx::query("INSERT INTO agent_chat_events (conversation_id,cursor,events_json,run_start,created_ms) VALUES ($1,$2,'[]',0,$3)")
            .bind(id).bind(cursor).bind(created)
            .execute(fixture.database().pool()).await.unwrap();
    }
}

fn missing(error: ConnectError) {
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.message.as_deref(), Some("conversation not found"));
}

#[tokio::test]
async fn archived_agent_isolation_refuses_all_active_rpc_paths() {
    let fixture = AgentFixture::new("archive-rpcs").await;
    seed(&fixture, "archived", "native").await;
    let list = handle_agent_chat_list(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatListRequest::default(),
    )
    .await
    .unwrap()
    .body;
    assert!(!list.conversations.iter().any(|row| row.id == "archived"));
    for id in ["archived", "nonexistent"] {
        missing(
            handle_agent_chat_snapshot(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatSnapshotRequest {
                    conversation_id: id.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err(),
        );
        missing(
            handle_agent_chat_events(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatEventsRequest {
                    conversation_id: id.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err(),
        );
        missing(
            handle_agent_chat_submit(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatSubmitRequest {
                    conversation_id: id.into(),
                    request_id: "submit".into(),
                    text: "never execute".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err(),
        );
        missing(
            handle_agent_chat_configure(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatConfigureRequest {
                    conversation_id: id.into(),
                    title: Some("changed".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err(),
        );
        missing(
            handle_agent_chat_abort(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatAbortRequest {
                    conversation_id: id.into(),
                    request_id: "stop".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err(),
        );
        missing(
            handle_agent_chat_withdraw(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatWithdrawRequest {
                    conversation_id: id.into(),
                    request_id: "withdraw".into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err(),
        );
        missing(
            handle_agent_chat_delete(
                &fixture.core,
                &fixture.caller,
                proto::AgentChatDeleteRequest {
                    conversation_id: id.into(),
                    ..Default::default()
                },
            )
            .await
            .unwrap_err(),
        );
    }
    let title: String =
        sqlx::query_scalar("SELECT title FROM agent_conversations WHERE id='archived'")
            .fetch_one(fixture.database().pool())
            .await
            .unwrap();
    assert_eq!(title, "saved");
}

#[tokio::test]
async fn archived_agent_isolation_prune_preserves_native_history_and_active_retention() {
    let fixture = AgentFixture::new("archive-prune").await;
    seed(&fixture, "archived", "native").await;
    seed(&fixture, "active", "omp").await;
    sqlx::query("INSERT INTO agent_entries (conversation_id,seq,entry_json,created_ms) VALUES ('archived',1,'{\"retained\":true}',0)").execute(fixture.database().pool()).await.unwrap();
    sqlx::query("INSERT INTO agent_credentials (provider,kind,identity_key,label,data_json,created_ms,updated_ms) VALUES ('old','api_key','old','old','encrypted-bytes',0,0)").execute(fixture.database().pool()).await.unwrap();
    assert_eq!(chat_events::prune(fixture.database()).await.unwrap(), 2);
    for (id, expected) in [("archived", vec![1, 2, 3, 4]), ("active", vec![3, 4])] {
        let cursors: Vec<i64> = sqlx::query_scalar(
            "SELECT cursor FROM agent_chat_events WHERE conversation_id=$1 ORDER BY cursor",
        )
        .bind(id)
        .fetch_all(fixture.database().pool())
        .await
        .unwrap();
        assert_eq!(cursors, expected);
    }
    let entry: String =
        sqlx::query_scalar("SELECT entry_json FROM agent_entries WHERE conversation_id='archived'")
            .fetch_one(fixture.database().pool())
            .await
            .unwrap();
    assert_eq!(entry, "{\"retained\":true}");
    let credential: String =
        sqlx::query_scalar("SELECT data_json FROM agent_credentials WHERE identity_key='old'")
            .fetch_one(fixture.database().pool())
            .await
            .unwrap();
    assert_eq!(credential, "encrypted-bytes");
}

#[tokio::test]
async fn archived_agent_isolation_relay_ignores_fabricated_archive_notices() {
    // Local notices exercise the same relay target check without a Postgres listener.
    if db_support::running_on_postgres() {
        return;
    }
    let fixture = AgentFixture::new("archive-relay").await;
    seed(&fixture, "archived", "native").await;
    seed(&fixture, "active", "omp").await;
    let notifier = Arc::clone(&fixture.core.services.agent.cluster.notifier);
    let (sender, mut updates) = tokio::sync::mpsc::unbounded_channel();
    let _subscription = fixture
        .core
        .services
        .buses
        .agent_chat_bus
        .subscribe(move |update| {
            let _ = sender.send((update.conversation_id.clone(), update.seq));
        });
    fixture.core.services.agent.join_cluster();
    tokio::task::yield_now().await;
    notifier.notify(CHAT_CHANNEL, "archived:1").await;
    notifier.notify(CHAT_CHANNEL, "nonexistent:1").await;
    notifier.notify(CHAT_CHANNEL, "active:1").await;
    let first = tokio::time::timeout(Duration::from_secs(5), updates.recv())
        .await
        .unwrap()
        .unwrap();
    fixture.core.services.agent.leave_cluster().await;
    assert_eq!(first, ("active".into(), 1));
}
