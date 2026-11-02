//! Settings persistence, immutable placement and worker epoch recovery.
//! These cases share OMP chat fixtures without native provider calls.
//! Run settlement is observed through the committed summary bus.

use super::*;
use omp::rpc_settings::{handle_agent_omp_settings_get, handle_agent_omp_settings_set};

#[tokio::test]
async fn omp_chat_settings_configure_and_delete_are_durable() {
    let fixture = AgentFixture::new("omp-settings").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let id = create(&fixture).await;
    let setting = OmpScopedSettings {
        scope: OmpSettingsScope::Machine {
            worker_fp: WORKER_A.into(),
        },
        values: OmpSettingValues {
            model: Some("provider/model".into()),
            thinking_level: Some("low".into()),
        },
    };
    handle_agent_omp_settings_set(
        &fixture.core,
        &fixture.caller,
        proto::AgentOmpSettingsSetRequest {
            scoped_json: serde_json::to_string(&setting).unwrap(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let response = handle_agent_omp_settings_get(
        &fixture.core,
        &fixture.caller,
        proto::AgentOmpSettingsGetRequest::default(),
    )
    .await
    .unwrap()
    .body;
    assert_eq!(
        serde_json::from_str::<OmpSettingsView>(&response.settings_json)
            .unwrap()
            .scopes,
        vec![setting]
    );
    for value in ["high", ""] {
        let response = handle_agent_chat_configure(
            &fixture.core,
            &fixture.caller,
            proto::AgentChatConfigureRequest {
                conversation_id: id.clone(),
                thinking_level: Some(value.into()),
                ..Default::default()
            },
        )
        .await
        .unwrap()
        .body;
        assert_eq!(
            conversation_from_proto(&response).thinking_level,
            (!value.is_empty()).then(|| value.into())
        );
    }
    let error = handle_agent_chat_configure(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatConfigureRequest {
            conversation_id: id.clone(),
            cwd: Some("/else".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::FailedPrecondition);
    submit(&fixture, &id, "req").await;
    receive(&mut downstream).await;
    handle_agent_chat_delete(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatDeleteRequest {
            conversation_id: id.clone(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        omp::store::find(&fixture.core.services.db, &id)
            .await
            .unwrap()
            .is_none()
    );
    let count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM agent_chat_events WHERE conversation_id=$1")
            .bind(id)
            .fetch_one(fixture.core.services.db.pool())
            .await
            .unwrap();
    assert_eq!(count, 0);
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpClose(_)
    ));
}

#[tokio::test]
async fn omp_chat_worker_restart_fails_the_turn_but_reconnect_does_not() {
    let fixture = AgentFixture::new("omp-restart").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let id = create(&fixture).await;
    submit(&fixture, &id, "req").await;
    receive(&mut downstream).await;
    let (sender, mut updates) = mpsc::unbounded_channel();
    let _subscription = fixture
        .core
        .services
        .buses
        .agent_conversation_bus
        .subscribe(move |update| {
            let _ = sender.send(update.clone());
        });
    let (handle, _receiver) = register_worker(&fixture, "worker-epoch");
    fixture.core.services.omp.on_ready(&handle);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), updates.recv())
            .await
            .is_err()
    );
    assert_eq!(
        snapshot(&fixture, &id).await.1.run_state,
        AgentRunState::Running
    );
    let (handle, _receiver) = register_worker(&fixture, "worker-epoch-2");
    fixture.core.services.omp.on_ready(&handle);
    let update = tokio::time::timeout(Duration::from_secs(5), updates.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        update.conversation.unwrap().run_state,
        AgentRunState::Failed
    );
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert_eq!(transcript.run_state, AgentRunState::Failed);
    assert!(transcript.items.iter().any(|item| matches!(item,TranscriptItem::Notice {body,..} if body == "The worker restarted during this turn.")));
}
