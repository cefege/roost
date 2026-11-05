//! Private OMP host-service routing: one device tab, one live transport, one
//! consuming reply, and receipts fenced by the canonical chat sequence.

use super::host_routing_support::{
    ServiceRequest, admit_input, caller_on, open_view, seen_bus, service_request,
    set_live_transport,
};
use super::*;
use roost_coord::agent::omp::host_services;
use roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui;
use roost_coord::events::bus_messages::UiBusMsg;
use roost_coord::sync_ws::feed::ui::{UiViewer, ui_bus_frame};
use serde_json::json;

#[tokio::test]
async fn host_service_requests_reach_only_the_admitting_tab_and_never_persist_their_value() {
    let fixture = AgentFixture::new("omp-host-service-route").await;
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
    host_services::accept_request(
        &fixture.core,
        WORKER_A,
        service_request(
            &fixture,
            ServiceRequest {
                conversation: &conversation_id,
                view: "view-a",
                transport: "nonce-a",
                service: "svc-1",
                origin: "input-1",
                kind: "copy_text",
                value: "private-clipboard-value",
                cancelled: false,
            },
        ),
    )
    .await;
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    let UiBusMsg::OmpHostService {
        fingerprint,
        tab_id,
        frame,
    } = &messages[0]
    else {
        panic!("expected a host service frame");
    };
    assert_eq!(fingerprint, fixture.caller.principal.fingerprint());
    assert_eq!(tab_id, "tab-a");
    assert_eq!(frame.kind, "copy_text");
    assert_eq!(frame.value, "private-clipboard-value");
    assert!(!frame.cancelled);

    let other_tab = UiViewer::browser("socket-b", "fingerprint", "tab-b");
    assert!(
        ui_bus_frame(&messages[0], &other_tab).is_none(),
        "another tab of the same device must not receive the effect"
    );
    let admitting = UiViewer::browser("socket-a", fixture.caller.principal.fingerprint(), "tab-a");
    assert!(ui_bus_frame(&messages[0], &admitting).is_some());

    let transcript = serde_json::to_string(&snapshot(&fixture, &conversation_id).await.1).unwrap();
    assert!(!transcript.contains("private-clipboard-value"));
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM omp_host_services WHERE conversation_id=$1")
            .bind(&conversation_id)
            .fetch_one(fixture.database().pool())
            .await
            .unwrap();
    assert_eq!(stored, 1, "only routing metadata is persisted");

    host_services::accept_request(
        &fixture.core,
        WORKER_A,
        service_request(
            &fixture,
            ServiceRequest {
                conversation: &conversation_id,
                view: "view-a",
                transport: "nonce-a",
                service: "svc-1",
                origin: "input-1",
                kind: "copy_text",
                value: "",
                cancelled: true,
            },
        ),
    )
    .await;
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages.len(), 2);
    let UiBusMsg::OmpHostService { frame, .. } = &messages[1] else {
        panic!("expected a cancellation frame");
    };
    assert!(frame.cancelled);
    assert!(frame.value.is_empty());
    assert_eq!(frame.kind, "copy_text");
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM omp_host_services WHERE conversation_id=$1")
            .bind(&conversation_id)
            .fetch_one(fixture.database().pool())
            .await
            .unwrap();
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn host_replies_consume_once_for_the_initiating_tab_and_live_transport_only() {
    let fixture = AgentFixture::new("omp-host-service-reply").await;
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
    host_services::accept_request(
        &fixture.core,
        WORKER_A,
        service_request(
            &fixture,
            ServiceRequest {
                conversation: &conversation_id,
                view: "view-a",
                transport: "nonce-a",
                service: "svc-2",
                origin: "input-1",
                kind: "open_url",
                value: "https://example.test/report",
                cancelled: false,
            },
        ),
    )
    .await;

    let reply = |caller: &roost_coord::coord_core::Caller,
                 view: &str,
                 transport: &str,
                 request_id: &str| proto::AgentOmpUiRequest {
        conversation_id: conversation_id.clone(),
        request_id: request_id.into(),
        action_json: json!({
            "method": "host_reply",
            "params": {"serviceId": "svc-2", "accepted": true},
        })
        .to_string(),
        view_id: view.into(),
        tab_id: caller.tab_id.clone().unwrap(),
        transport_id: transport.into(),
        receipt: false,
        ..Default::default()
    };

    let other_tab = caller_on(&fixture, "tab-b");
    let error = handle_agent_omp_ui(
        &fixture.core,
        &other_tab,
        reply(&other_tab, "view-a", "nonce-a", "reply-x"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::FailedPrecondition);
    let error = handle_agent_omp_ui(
        &fixture.core,
        &caller,
        reply(&caller, "view-b", "nonce-a", "reply-y"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::FailedPrecondition);

    // A new native generation cannot consume the old prompt.
    fixture
        .exec("UPDATE omp_sessions SET omp_ui_transport_id='nonce-b'")
        .await;
    let error = handle_agent_omp_ui(
        &fixture.core,
        &caller,
        reply(&caller, "view-a", "nonce-a", "reply-z"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::FailedPrecondition);

    fixture
        .exec("UPDATE omp_sessions SET omp_ui_transport_id='nonce-a'")
        .await;
    handle_agent_omp_ui(
        &fixture.core,
        &caller,
        reply(&caller, "view-a", "nonce-a", "reply-1"),
    )
    .await
    .expect("the initiating tab consumes its prompt");
    let CoordWorkerDownstream::OmpUi(frame) = receive(&mut downstream).await else {
        panic!("expected the host reply to reach the worker");
    };
    let sent: serde_json::Value = serde_json::from_str(&frame.action_json).unwrap();
    assert_eq!(sent["method"], "host_reply");
    assert_eq!(sent["params"]["serviceId"], "svc-2");
    assert_eq!(sent["params"]["accepted"], true);
    assert!(sent["params"].get("viewId").is_none());
    assert_eq!(frame.view_id, "view-a");
    assert_eq!(frame.tab_id, "tab-a");
    assert_eq!(frame.transport_id, "nonce-a");
    assert_eq!(
        frame.client_id,
        format!("{}:view-a", fixture.caller.principal.fingerprint())
    );
    assert!(!frame.receipt);

    let error = handle_agent_omp_ui(
        &fixture.core,
        &caller,
        reply(&caller, "view-a", "nonce-a", "reply-2"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, connectrpc::ErrorCode::FailedPrecondition);
    assert!(
        downstream.try_recv().is_err(),
        "a duplicate reply re-executed"
    );
}

#[tokio::test]
async fn ui_action_receipts_are_private_current_generation_and_fenced_by_the_chat_sequence() {
    let fixture = AgentFixture::new("omp-ui-receipt-fence").await;
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

    host_services::accept_receipt(
        &fixture.core,
        WORKER_A,
        proto::WOmpUiActionReceipt {
            conversation_id: conversation_id.clone(),
            origin_request_id: "input-1".into(),
            client_id: format!("{}:view-a", fixture.caller.principal.fingerprint()),
            view_id: "view-a".into(),
            transport_id: "nonce-a".into(),
            error: String::new(),
            ..Default::default()
        },
    )
    .await;
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages.len(), 1);
    let UiBusMsg::OmpUiActionReceipt {
        fingerprint,
        tab_id,
        frame,
    } = &messages[0]
    else {
        panic!("expected an action receipt");
    };
    assert_eq!(fingerprint, fixture.caller.principal.fingerprint());
    assert_eq!(tab_id, "tab-a");
    assert_eq!(frame.origin_request_id, "input-1");
    assert!(frame.error.is_empty());
    assert!(
        frame.after_seq > 0,
        "the receipt must fence the reset frame"
    );
    assert!(ui_bus_frame(&messages[0], &UiViewer::browser("s", "f", "tab-b")).is_none());

    host_services::accept_receipt(
        &fixture.core,
        WORKER_A,
        proto::WOmpUiActionReceipt {
            conversation_id: conversation_id.clone(),
            origin_request_id: "input-1".into(),
            client_id: format!("{}:view-a", fixture.caller.principal.fingerprint()),
            view_id: "view-a".into(),
            transport_id: "nonce-b".into(),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "a stale transport receipt reached the browser"
    );

    // A replay of an admitted receipt-tracked request neither re-executes the
    // effect nor leaves its consumer waiting.
    handle_agent_omp_ui(
        &fixture.core,
        &caller,
        proto::AgentOmpUiRequest {
            conversation_id: conversation_id.clone(),
            request_id: "input-1".into(),
            action_json: json!({"method":"input","params":{"data":"\u{1b}[A"}}).to_string(),
            view_id: "view-a".into(),
            tab_id: "tab-a".into(),
            transport_id: "nonce-a".into(),
            receipt: true,
            ..Default::default()
        },
    )
    .await
    .expect("a duplicate request is answered");
    assert!(downstream.try_recv().is_err(), "a duplicate re-executed");
    let messages = seen.lock().unwrap().clone();
    assert_eq!(messages.len(), 2);
    let UiBusMsg::OmpUiActionReceipt { frame, .. } = &messages[1] else {
        panic!("expected a duplicate fence receipt");
    };
    assert_eq!(frame.origin_request_id, "input-1");
    assert!(frame.error.contains("duplicate"));
}
