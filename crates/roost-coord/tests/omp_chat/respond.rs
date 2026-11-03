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
