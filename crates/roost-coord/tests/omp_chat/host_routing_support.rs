//! Shared setup for the private OMP host-service routing tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::{AgentFixture, CoordWorkerDownstream, WORKER_A, proto, receive};
use roost_coord::agent::omp::rpc_ui::handle_agent_omp_ui;
use roost_coord::events::bus::Subscription;
use roost_coord::events::bus_messages::UiBusMsg;
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) fn seen_bus(
    fixture: &AgentFixture,
) -> (Arc<Mutex<Vec<UiBusMsg>>>, Subscription<UiBusMsg>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&seen);
    let subscription = fixture
        .core
        .services
        .buses
        .ui_bus
        .subscribe(move |message| captured.lock().unwrap().push(message.clone()));
    (seen, subscription)
}

pub(crate) fn caller_on(fixture: &AgentFixture, tab: &str) -> roost_coord::coord_core::Caller {
    let mut caller = fixture.caller.clone();
    caller.tab_id = Some(tab.to_owned());
    caller
}

pub(crate) async fn open_view(
    fixture: &AgentFixture,
    caller: &roost_coord::coord_core::Caller,
    conversation_id: &str,
    view: &str,
    transport: &str,
    request_id: &str,
    downstream: &mut tokio::sync::mpsc::UnboundedReceiver<CoordWorkerDownstream>,
) {
    handle_agent_omp_ui(
        &fixture.core,
        caller,
        proto::AgentOmpUiRequest {
            conversation_id: conversation_id.into(),
            request_id: request_id.into(),
            action_json: json!({"method":"open","params":{"cols":80,"rows":24,"dark":true,"reduceMotion":false}}).to_string(),
            view_id: view.into(),
            tab_id: caller.tab_id.clone().unwrap(),
            transport_id: transport.into(),
            receipt: false,
            ..Default::default()
        },
    )
    .await
    .expect("view open admitted");
    assert!(matches!(
        receive(downstream).await,
        CoordWorkerDownstream::OmpUi(_)
    ));
}

pub(crate) async fn set_live_transport(
    fixture: &AgentFixture,
    conversation_id: &str,
    transport: &str,
    downstream: &mut tokio::sync::mpsc::UnboundedReceiver<CoordWorkerDownstream>,
) {
    fixture.core.services.omp.receive(
        WORKER_A,
        CoordWorkerUpstream::OmpEvents(proto::WOmpEvents {
            conversation_id: conversation_id.into(),
            run_id: "worker-run".into(),
            first_seq: 1,
            events_json: vec![
                serde_json::to_string(&OmpWorkerEvent::UiReset {
                    transport_id: Some(transport.into()),
                })
                .unwrap(),
            ],
            ..Default::default()
        }),
    );
    assert!(matches!(
        receive(downstream).await,
        CoordWorkerDownstream::OmpEventsAck(_)
    ));
    for _ in 0..200 {
        let current: String = sqlx::query_scalar(
            "SELECT omp_ui_transport_id FROM omp_sessions WHERE conversation_id=$1",
        )
        .bind(conversation_id)
        .fetch_one(fixture.database().pool())
        .await
        .unwrap();
        if current == transport {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the live transport was not persisted");
}

pub(crate) async fn admit_input(
    fixture: &AgentFixture,
    caller: &roost_coord::coord_core::Caller,
    conversation_id: &str,
    view: &str,
    transport: &str,
    request_id: &str,
    downstream: &mut tokio::sync::mpsc::UnboundedReceiver<CoordWorkerDownstream>,
) {
    handle_agent_omp_ui(
        &fixture.core,
        caller,
        proto::AgentOmpUiRequest {
            conversation_id: conversation_id.into(),
            request_id: request_id.into(),
            action_json: json!({"method":"input","params":{"data":"\u{1b}[A"}}).to_string(),
            view_id: view.into(),
            tab_id: caller.tab_id.clone().unwrap(),
            transport_id: transport.into(),
            receipt: true,
            ..Default::default()
        },
    )
    .await
    .expect("input admitted");
    assert!(matches!(
        receive(downstream).await,
        CoordWorkerDownstream::OmpUi(frame) if frame.receipt
    ));
}

pub(crate) struct ServiceRequest<'a> {
    pub conversation: &'a str,
    pub view: &'a str,
    pub transport: &'a str,
    pub service: &'a str,
    pub origin: &'a str,
    pub kind: &'a str,
    pub value: &'a str,
    pub cancelled: bool,
}

pub(crate) fn service_request(
    fixture: &AgentFixture,
    request: ServiceRequest<'_>,
) -> proto::WOmpHostServiceRequest {
    proto::WOmpHostServiceRequest {
        conversation_id: request.conversation.into(),
        service_id: request.service.into(),
        origin_request_id: request.origin.into(),
        client_id: format!(
            "{}:{}",
            fixture.caller.principal.fingerprint(),
            request.view
        ),
        view_id: request.view.into(),
        transport_id: request.transport.into(),
        kind: request.kind.into(),
        value: request.value.into(),
        cancelled: request.cancelled,
        ..Default::default()
    }
}
