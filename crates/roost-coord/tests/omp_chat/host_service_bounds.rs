//! The per-conversation bound on pending browser service prompts.

use super::host_routing_support::{
    ServiceRequest, admit_input, caller_on, open_view, seen_bus, service_request,
    set_live_transport,
};
use super::*;
use roost_coord::agent::omp::host_services;

#[tokio::test]
async fn host_service_prompts_are_bounded_per_conversation() {
    let fixture = AgentFixture::new("omp-host-service-bound").await;
    let (_worker, mut downstream) = register_worker(&fixture, "worker-epoch");
    let caller = caller_on(&fixture, "tab-a");
    let conversation_id = create(&fixture).await;
    open_view(
        &fixture,
        &caller,
        &conversation_id,
        "view-a",
        "",
        "open-1",
        &mut downstream,
    )
    .await;
    set_live_transport(&fixture, &conversation_id, "nonce-a", &mut downstream).await;
    admit_input(
        &fixture,
        &caller,
        &conversation_id,
        "view-a",
        "nonce-a",
        "input-1",
        &mut downstream,
    )
    .await;
    let (seen, _subscription) = seen_bus(&fixture);
    for index in 0..33 {
        host_services::accept_request(
            &fixture.core,
            WORKER_A,
            service_request(
                &fixture,
                ServiceRequest {
                    conversation: &conversation_id,
                    view: "view-a",
                    transport: "nonce-a",
                    service: &format!("svc-{index}"),
                    origin: "input-1",
                    kind: "copy_text",
                    value: "bounded",
                    cancelled: false,
                },
            ),
        )
        .await;
    }
    assert_eq!(seen.lock().unwrap().len(), 32);
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM omp_host_services WHERE conversation_id=$1")
            .bind(&conversation_id)
            .fetch_one(fixture.database().pool())
            .await
            .unwrap();
    assert_eq!(stored, 32);
}

#[tokio::test]
async fn an_untracked_typed_edit_failure_receipt_is_delivered_once_and_never_credits_success() {
    use roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui;
    use roost_coord::events::bus_messages::UiBusMsg;
    use serde_json::json;

    let fixture = AgentFixture::new("omp-untyped-failure-receipt").await;
    let (_worker, mut downstream) = register_worker(&fixture, "worker-epoch");
    let caller = caller_on(&fixture, "tab-a");
    let conversation_id = create(&fixture).await;
    open_view(
        &fixture,
        &caller,
        &conversation_id,
        "view-a",
        "",
        "open-1",
        &mut downstream,
    )
    .await;
    set_live_transport(&fixture, &conversation_id, "nonce-a", &mut downstream).await;
    for request_id in ["edit-1", "edit-2"] {
        handle_agent_omp_ui(
            &fixture.core,
            &caller,
            proto::AgentOmpUiRequest {
                conversation_id: conversation_id.clone(),
                request_id: request_id.into(),
                action_json: json!({"method":"input","params":{"data":"x"}}).to_string(),
                view_id: "view-a".into(),
                tab_id: "tab-a".into(),
                transport_id: "nonce-a".into(),
                receipt: false,
                ..Default::default()
            },
        )
        .await
        .expect("an untracked input is admitted");
        assert!(matches!(
            receive(&mut downstream).await,
            CoordWorkerDownstream::OmpUi(frame) if !frame.receipt
        ));
    }
    let (seen, _subscription) = seen_bus(&fixture);
    let client_id = format!("{}:view-a", fixture.caller.principal.fingerprint());
    let failure = |origin: &str, error: &str| proto::WOmpUiActionReceipt {
        conversation_id: conversation_id.clone(),
        origin_request_id: origin.into(),
        client_id: client_id.clone(),
        view_id: "view-a".into(),
        transport_id: "nonce-a".into(),
        error: error.into(),
        ..Default::default()
    };
    host_services::accept_receipt(
        &fixture.core,
        WORKER_A,
        failure("edit-1", "typed edit rejected"),
    )
    .await;
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    let UiBusMsg::OmpUiActionReceipt { frame, .. } = &messages[0] else {
        panic!("expected a private failure receipt");
    };
    assert_eq!(frame.origin_request_id, "edit-1");
    assert_eq!(frame.error, "typed edit rejected");

    host_services::accept_receipt(
        &fixture.core,
        WORKER_A,
        failure("edit-1", "typed edit rejected"),
    )
    .await;
    assert_eq!(seen.lock().unwrap().len(), 1, "a replay credited twice");

    host_services::accept_receipt(&fixture.core, WORKER_A, failure("edit-2", "")).await;
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "an untracked success credit reached the browser"
    );
}
