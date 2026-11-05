//! Consumer-visible semantic UI lifecycle regressions through the real owner.
//! Native protocol barriers prove ordering without absence timers or mock echoes.
//! A stale transport must not corrupt the new document or acknowledge its frames.

#[path = "ui_peer.rs"]
mod ui_peer;
use super::frame;
use roost_proto::{DOmpClose, DOmpEventsAck, DOmpUi, WOmpEvents};
use roost_protocol::wire::{
    agent_chat::{OmpWorkerEvent, semantic_ui::OmpUiDocument},
    coord_worker::CoordWorkerUpstream,
};
use roost_worker::{
    link_ports::OmpPort,
    omp::OmpOwner,
    uplink::{self, UplinkReceiver},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::sync::mpsc;
use ui_peer::{Control, PeerRequest, UiLauncher};

#[path = "ui/host_services.rs"]
mod host_services;
#[path = "ui/receipts.rs"]
mod receipts;
#[path = "ui/refresh.rs"]
mod refresh;

fn action(request_id: &str, client: &str, method: &str, params: Value) -> DOmpUi {
    DOmpUi {
        conversation_id: "c1".into(),
        request_id: request_id.into(),
        client_id: client.into(),
        cwd: "/work".into(),
        action_json: json!({"method":method,"params":params}).to_string(),
        ..Default::default()
    }
}

const TAB: &str = "tab-1";

/// One view-owned action carrying the worker-issued transport token.
fn view_action(
    request_id: &str,
    client: &str,
    view: &str,
    transport: &str,
    method: &str,
    params: Value,
) -> DOmpUi {
    DOmpUi {
        view_id: view.into(),
        transport_id: transport.into(),
        ..action(request_id, client, method, params)
    }
}

fn open(request_id: &str, client: &str, view: &str, params: Value) -> DOmpUi {
    DOmpUi {
        view_id: view.into(),
        tab_id: TAB.into(),
        ..action(request_id, client, "open", params)
    }
}

async fn peer_request(receiver: &mut mpsc::UnboundedReceiver<PeerRequest>) -> PeerRequest {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

async fn next_upstream(receiver: &mut UplinkReceiver) -> CoordWorkerUpstream {
    tokio::time::timeout(Duration::from_secs(5), receiver.recv_delivery())
        .await
        .expect("frame deadline")
        .expect("uplink")
        ._take_unconfirmed_frame()
        .expect("unconfirmed frame")
}

async fn until(
    receiver: &mut UplinkReceiver,
    document: &mut OmpUiDocument,
    predicate: impl Fn(&OmpWorkerEvent) -> bool,
) -> WOmpEvents {
    loop {
        let batch = frame(receiver).await;
        let mut found = false;
        for value in &batch.events_json {
            let event: OmpWorkerEvent = serde_json::from_str(value).unwrap();
            match &event {
                OmpWorkerEvent::UiReset { .. } => *document = OmpUiDocument::default(),
                OmpWorkerEvent::Ui { message } => document.apply_message(message).unwrap(),
                _ => {}
            }
            found |= predicate(&event);
        }
        if found {
            return batch;
        }
    }
}

fn reset_transports(batch: &WOmpEvents) -> Vec<Option<String>> {
    batch
        .events_json
        .iter()
        .filter_map(
            |value| match serde_json::from_str::<OmpWorkerEvent>(value).unwrap() {
                OmpWorkerEvent::UiReset { transport_id } => Some(transport_id),
                _ => None,
            },
        )
        .collect()
}

fn ui_frame(event: &OmpWorkerEvent) -> bool {
    matches!(event,OmpWorkerEvent::Ui {message} if message["verb"]=="f")
}

fn close(owner: &impl OmpPort) {
    owner.close(DOmpClose {
        conversation_id: "c1".into(),
        ..Default::default()
    });
}

#[tokio::test]
async fn closing_one_view_preserves_the_other_and_credits_wait_for_durable_ack() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "generation-one",
        "device:a",
        "view-a",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/open");
    let delivered = until(&mut receiver, &mut document, ui_frame).await;
    owner.ui(open(
        "second-view",
        "device:b",
        "view-b",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    owner.ui(view_action(
        "close-first",
        "device:a",
        "view-a",
        "generation-one",
        "close",
        json!({}),
    ));
    owner.ui(view_action(
        "barrier",
        "device:b",
        "view-b",
        "generation-one",
        "input",
        json!({"data":"still editable","sessionId":"forged-other-session"}),
    ));
    let barrier = peer_request(&mut requests).await;
    assert_eq!(barrier.method, "_omp/ui/input");
    assert_eq!(barrier.params["sessionId"], "live-handle");
    assert_eq!(barrier.params["transportId"], "generation-one");
    assert_eq!(
        reset_transports(&delivered),
        vec![Some("generation-one".to_string())]
    );
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    owner.ack(DOmpEventsAck {
        conversation_id: "c1".into(),
        run_id: delivered.run_id,
        seq: delivered.first_seq + delivered.events_json.len() as u64 - 1,
        ..Default::default()
    });
    let credit = peer_request(&mut requests).await;
    assert_eq!(credit.method, "_omp/ui/event");
    assert_eq!(credit.params["event"], json!({"ev":"ack","sf":"s:0","s":1}));
    launcher.control(Control::State {
        streaming: true,
        resume_id: "live-handle".into(),
    });
    until(&mut receiver, &mut document, |event| {
        matches!(event, OmpWorkerEvent::TurnStarted { .. })
    })
    .await;
    owner.ui(view_action(
        "close-last",
        "device:b",
        "view-b",
        "generation-one",
        "close",
        json!({}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/close");
    launcher.control(Control::State {
        streaming: false,
        resume_id: "live-handle".into(),
    });
    let ended = until(&mut receiver, &mut document, |event| {
        matches!(
            event,
            OmpWorkerEvent::TurnEnded {
                outstanding: 0,
                error: None,
                ..
            }
        )
    })
    .await;
    assert_eq!(reset_transports(&ended), vec![None]);
    close(&owner);
}

#[tokio::test]
async fn stale_transport_frames_cannot_change_the_reconnected_document() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "old-generation",
        "device:a",
        "view-a",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    until(&mut receiver, &mut document, ui_frame).await;
    owner.ui(open(
        "new-generation",
        "device:a",
        "view-a",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false, "reset": true
        }),
    ));
    let reopened = peer_request(&mut requests).await;
    assert_eq!(reopened.params["transportId"], "new-generation");
    until(&mut receiver, &mut document, ui_frame).await;
    launcher.control(Control::Frame {
        transport_id: "old-generation".into(),
        text: "stale context leaked".into(),
    });
    until(&mut receiver, &mut document, |event| {
        matches!(event, OmpWorkerEvent::UiState { .. })
    })
    .await;
    assert_eq!(document.surfaces["s:0"].sequence, 1);
    assert_eq!(document.surfaces["s:0"].root.c[0].c[0].p["text"], "fresh");
    close(&owner);
}

#[tokio::test]
async fn a_stale_expected_transport_cannot_reach_the_live_view() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "live-generation",
        "device:a",
        "view-a",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    until(&mut receiver, &mut document, ui_frame).await;
    owner.ui(view_action(
        "stale-action",
        "device:a",
        "view-a",
        "previous-generation",
        "input",
        json!({"data":"stale"}),
    ));
    owner.ui(view_action(
        "barrier",
        "device:a",
        "view-a",
        "live-generation",
        "input",
        json!({"data":"fresh"}),
    ));
    let barrier = peer_request(&mut requests).await;
    assert_eq!(barrier.method, "_omp/ui/input");
    assert_eq!(barrier.params["data"], "fresh");
    close(&owner);
}
