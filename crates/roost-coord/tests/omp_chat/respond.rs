//! ACP answers are preflighted against durable pending requests and deduplicated.

use super::*;
use roost_coord::agent::omp::rpc::handle_agent_omp_respond;
use serde_json::json;

fn interaction_frame(conversation_id: &str, seq: u64, event: OmpWorkerEvent) -> proto::WOmpEvents {
    proto::WOmpEvents {
        conversation_id: conversation_id.into(),
        run_id: "worker-run".into(),
        first_seq: seq,
        events_json: vec![serde_json::to_string(&event).unwrap()],
        ..Default::default()
    }
}
#[tokio::test]
async fn omp_new_actor_run_clears_pending_interactions_from_reloaded_transcript() {
    let fixture = AgentFixture::new("omp-interaction-run-change").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let conversation_id = create(&fixture).await;
    let interaction = OmpInteraction {
        id: "old-worker-run:1:17".into(),
        request: json!({"kind":"form","mode":"form","requestedSchema":{"type":"object","properties":{}}}),
    };
    fixture.core.services.omp.receive(
        WORKER_A,
        CoordWorkerUpstream::OmpEvents(interaction_frame(
            &conversation_id,
            1,
            OmpWorkerEvent::Interaction { interaction },
        )),
    );
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpEventsAck(_)
    ));
    assert_eq!(
        snapshot(&fixture, &conversation_id)
            .await
            .1
            .omp_interactions
            .len(),
        1
    );

    let new_run = proto::WOmpEvents {
        conversation_id: conversation_id.clone(),
        run_id: "new-worker-run".into(),
        first_seq: 1,
        events_json: vec![
            serde_json::to_string(&OmpWorkerEvent::SessionReady {
                acp_session_id: "s-new".into(),
                config_options: json!([]),
                mode: None,
            })
            .unwrap(),
        ],
        ..Default::default()
    };
    fixture
        .core
        .services
        .omp
        .receive(WORKER_A, CoordWorkerUpstream::OmpEvents(new_run));
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpEventsAck(_)
    ));
    assert!(
        snapshot(&fixture, &conversation_id)
            .await
            .1
            .omp_interactions
            .is_empty()
    );
}

#[tokio::test]
async fn omp_worker_restart_clears_idle_pending_interaction() {
    let fixture = AgentFixture::new("omp-interaction-idle-restart").await;
    let (_handle, mut downstream) = register_worker(&fixture, "old-worker-epoch");
    let conversation_id = create(&fixture).await;
    let interaction = OmpInteraction {
        id: "old-worker-run:1:21".into(),
        request: json!({"kind":"form","mode":"form","requestedSchema":{"type":"object","properties":{}}}),
    };
    fixture.core.services.omp.receive(
        WORKER_A,
        CoordWorkerUpstream::OmpEvents(interaction_frame(
            &conversation_id,
            1,
            OmpWorkerEvent::Interaction { interaction },
        )),
    );
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpEventsAck(_)
    ));
    assert_eq!(
        snapshot(&fixture, &conversation_id)
            .await
            .1
            .omp_interactions
            .len(),
        1
    );

    let (new_handle, _new_downstream) = register_worker(&fixture, "new-worker-epoch");
    fixture.core.services.omp.on_ready(&new_handle);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if snapshot(&fixture, &conversation_id)
                .await
                .1
                .omp_interactions
                .is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("worker restart clears the obsolete interaction");
}

#[tokio::test]
async fn omp_response_validates_pending_form_deduplicates_and_rejects_stale_item() {
    let fixture = AgentFixture::new("omp-interaction-response").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let conversation_id = create(&fixture).await;
    let interaction = OmpInteraction {
        id: "worker-run:1:91".into(),
        request: json!({
            "kind":"form",
            "mode":"form",
            "requestedSchema":{
                "type":"object","required":["ok"],
                "properties":{"ok":{"type":"boolean"}}
            }
        }),
    };
    fixture.core.services.omp.receive(
        WORKER_A,
        CoordWorkerUpstream::OmpEvents(interaction_frame(
            &conversation_id,
            1,
            OmpWorkerEvent::Interaction {
                interaction: interaction.clone(),
            },
        )),
    );
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpEventsAck(_)
    ));
    assert_eq!(
        snapshot(&fixture, &conversation_id)
            .await
            .1
            .omp_interactions,
        vec![interaction.clone()]
    );

    let invalid = handle_agent_omp_respond(
        &fixture.core,
        &fixture.caller,
        proto::AgentOmpRespondRequest {
            conversation_id: conversation_id.clone(),
            item_id: interaction.id.clone(),
            action: "accept".into(),
            content_json: r#"{"ok":"yes"}"#.into(),
            request_id: "answer-1".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(invalid.code, connectrpc::ErrorCode::InvalidArgument);
    assert!(downstream.try_recv().is_err());

    let answer = proto::AgentOmpRespondRequest {
        conversation_id: conversation_id.clone(),
        item_id: interaction.id.clone(),
        action: "accept".into(),
        content_json: r#"{"ok":false}"#.into(),
        request_id: "answer-1".into(),
        ..Default::default()
    };
    handle_agent_omp_respond(&fixture.core, &fixture.caller, answer.clone())
        .await
        .unwrap();
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpRespond(response)
            if response.request_id == interaction.id && response.content_json == r#"{"ok":false}"#
    ));
    handle_agent_omp_respond(&fixture.core, &fixture.caller, answer)
        .await
        .unwrap();
    assert!(downstream.try_recv().is_err());

    fixture.core.services.omp.receive(
        WORKER_A,
        CoordWorkerUpstream::OmpEvents(interaction_frame(
            &conversation_id,
            2,
            OmpWorkerEvent::InteractionResolved {
                id: interaction.id.clone(),
            },
        )),
    );
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpEventsAck(_)
    ));
    assert!(
        snapshot(&fixture, &conversation_id)
            .await
            .1
            .omp_interactions
            .is_empty()
    );
    let stale = handle_agent_omp_respond(
        &fixture.core,
        &fixture.caller,
        proto::AgentOmpRespondRequest {
            conversation_id,
            item_id: interaction.id,
            action: "accept".into(),
            content_json: r#"{"ok":true}"#.into(),
            request_id: "answer-stale".into(),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(stale.code, connectrpc::ErrorCode::FailedPrecondition);
    assert!(downstream.try_recv().is_err());
}

#[tokio::test]
async fn omp_ui_opens_new_conversations_and_deduplicates_per_conversation() {
    let fixture = AgentFixture::new("omp-ui-admission-idempotency").await;
    let mut caller = fixture.caller.clone();
    caller.tab_id = Some("tab-a".into());
    let (_worker, mut downstream) = register_worker(&fixture, "worker-epoch");
    let fresh = create(&fixture).await;
    let first = create(&fixture).await;
    let second = create(&fixture).await;
    for conversation_id in [&first, &second] {
        fixture.core.services.omp.receive(
            WORKER_A,
            CoordWorkerUpstream::OmpEvents(interaction_frame(
                conversation_id,
                1,
                OmpWorkerEvent::SessionReady {
                    acp_session_id: "session-a".into(),
                    config_options: serde_json::json!([]),
                    mode: None,
                },
            )),
        );
        assert!(matches!(
            receive(&mut downstream).await,
            CoordWorkerDownstream::OmpEventsAck(_)
        ));
    }
    let action_json =
        r#"{"method":"open","params":{"cols":80,"rows":24,"dark":true,"reduceMotion":false}}"#;
    let request = |conversation_id: &str| proto::AgentOmpUiRequest {
        conversation_id: conversation_id.into(),
        request_id: "key-1".into(),
        action_json: action_json.into(),
        view_id: "view-a".into(),
        tab_id: "tab-a".into(),
        ..Default::default()
    };
    roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui(&fixture.core, &caller, request(&fresh))
        .await
        .expect("new conversation UI open admitted without a resume ID");
    let CoordWorkerDownstream::OmpUi(frame) = receive(&mut downstream).await else {
        panic!("expected new-conversation UI open");
    };
    assert_eq!(frame.conversation_id, fresh);
    assert!(frame.acp_session_id.is_empty());

    roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui(&fixture.core, &caller, request(&first))
        .await
        .expect("first UI action admitted");
    let CoordWorkerDownstream::OmpUi(frame) = receive(&mut downstream).await else {
        panic!("expected OMP UI action");
    };
    assert_eq!(frame.conversation_id, first);
    assert_eq!(frame.cwd, "/work");
    assert_eq!(frame.acp_session_id, "session-a");
    assert_eq!(
        frame.client_id,
        format!("{}:view-a", fixture.caller.principal.fingerprint())
    );

    roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui(&fixture.core, &caller, request(&first))
        .await
        .expect("duplicate UI action is idempotent");
    assert!(downstream.try_recv().is_err());

    roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui(&fixture.core, &caller, request(&second))
        .await
        .expect("request id is scoped to its conversation");
    let CoordWorkerDownstream::OmpUi(frame) = receive(&mut downstream).await else {
        panic!("expected OMP UI action for second conversation");
    };
    assert_eq!(frame.conversation_id, second);
    let error = roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui(
        &fixture.core,
        &caller,
        request("missing-conversation"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::NotFound);
    assert!(downstream.try_recv().is_err());
}

#[tokio::test]
async fn ui_state_persists_resume_identity_and_resets_history_without_losing_surface() {
    let fixture = AgentFixture::new("omp-ui-resume-state").await;
    let (_worker, mut downstream) = register_worker(&fixture, "worker-epoch");
    let conversation_id = create(&fixture).await;
    let events = [
        OmpWorkerEvent::SessionReady {
            acp_session_id: "live-handle".into(),
            config_options: serde_json::json!([]),
            mode: None,
        },
        OmpWorkerEvent::Ui {
            message: serde_json::json!({"verb":"o","body":{"id":"surface-a"}}),
        },
        OmpWorkerEvent::Update {
            update: serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"old history"}}),
        },
        OmpWorkerEvent::UiState {
            resume_id: "session-manager-id".into(),
            cwd: "/work/next".into(),
            streaming: false,
            reset_history: true,
        },
        OmpWorkerEvent::Update {
            update: serde_json::json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"new history"}}),
        },
    ];
    for (index, event) in events.into_iter().enumerate() {
        fixture.core.services.omp.receive(
            WORKER_A,
            CoordWorkerUpstream::OmpEvents(interaction_frame(
                &conversation_id,
                index as u64 + 1,
                event,
            )),
        );
        assert!(matches!(
            receive(&mut downstream).await,
            CoordWorkerDownstream::OmpEventsAck(_)
        ));
    }
    let conversation =
        roost_coord::agent::omp::store::find(&fixture.core.services.db, &conversation_id)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(
        conversation.acp_session_id.as_deref(),
        Some("session-manager-id")
    );
    assert_eq!(conversation.cwd, "/work/next");
    let transcript = snapshot(&fixture, &conversation_id).await.1;
    assert_eq!(transcript.items.len(), 1);
    assert!(transcript.omp_ui.is_some());
    let transcript_json = serde_json::to_string(&transcript).unwrap();
    assert!(transcript_json.contains("new history"));
    assert!(!transcript_json.contains("old history"));
}
