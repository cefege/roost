//! Browser-owned effect requests bound to the view and transport that filed them.
//! Replies are consumed once; a closing view withdraws the prompts it created.

use super::*;

#[tokio::test]
async fn host_services_are_private_bound_to_the_view_and_consumed_once() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "open-1",
        "device:a",
        "view-a",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    until(&mut receiver, &mut document, ui_frame).await;

    launcher.control(Control::Notify {
        method: "_omp/ui/host_service_request".into(),
        params: json!({
            "sessionId": "live-handle",
            "origin": {"requestId": "open-1", "viewId": "view-a", "transportId": "open-1"},
            "serviceId": "svc-1",
            "kind": "copy_text",
            "value": "private clipboard payload",
        }),
    });
    let CoordWorkerUpstream::OmpHostServiceRequest(request) = next_upstream(&mut receiver).await
    else {
        panic!("host service request frame");
    };
    assert!(!request.cancelled);
    assert_eq!(request.kind, "copy_text");
    assert_eq!(request.value, "private clipboard payload");
    assert_eq!(request.client_id, "device:a");
    assert_eq!(request.view_id, "view-a");
    assert_eq!(request.transport_id, "open-1");
    assert_eq!(request.origin_request_id, "open-1");

    owner.ui(view_action(
        "reply-1",
        "device:a",
        "view-a",
        "open-1",
        "host_reply",
        json!({"serviceId":"svc-1","accepted":false,"error":"permission denied"}),
    ));
    let reply = peer_request(&mut requests).await;
    assert_eq!(reply.method, "_omp/ui/host_service_reply");
    assert_eq!(reply.params["serviceId"], "svc-1");
    assert_eq!(reply.params["accepted"], false);
    assert_eq!(reply.params["error"], "permission denied");
    assert_eq!(reply.params["origin"]["requestId"], "open-1");
    assert_eq!(reply.params["origin"]["viewId"], "view-a");

    // A second reply for the consumed request must not reach native again.
    owner.ui(view_action(
        "reply-2",
        "device:a",
        "view-a",
        "open-1",
        "host_reply",
        json!({"serviceId":"svc-1","accepted":true}),
    ));
    owner.ui(view_action(
        "barrier",
        "device:a",
        "view-a",
        "open-1",
        "input",
        json!({"data":"barrier"}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    close(&owner);
}

#[tokio::test]
async fn a_reply_from_another_view_never_reaches_native_and_close_withdraws_prompts() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "open-2",
        "device:a",
        "view-a",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    until(&mut receiver, &mut document, ui_frame).await;

    launcher.control(Control::Notify {
        method: "_omp/ui/host_service_request".into(),
        params: json!({
            "sessionId": "live-handle",
            "origin": {"requestId": "open-2", "viewId": "view-a", "transportId": "open-2"},
            "serviceId": "svc-2",
            "kind": "open_url",
            "value": "https://example.com/private",
        }),
    });
    let CoordWorkerUpstream::OmpHostServiceRequest(request) = next_upstream(&mut receiver).await
    else {
        panic!("host service request frame");
    };
    assert_eq!(request.kind, "open_url");

    owner.ui(view_action(
        "forged-reply",
        "device:a",
        "view-b",
        "open-2",
        "host_reply",
        json!({"serviceId":"svc-2","accepted":true}),
    ));
    owner.ui(view_action(
        "barrier",
        "device:a",
        "view-a",
        "open-2",
        "input",
        json!({"data":"barrier"}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");

    owner.ui(view_action(
        "close-1",
        "device:a",
        "view-a",
        "open-2",
        "close",
        json!({}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/close");
    loop {
        match next_upstream(&mut receiver).await {
            CoordWorkerUpstream::OmpHostServiceRequest(request) => {
                assert!(request.cancelled);
                assert_eq!(request.service_id, "svc-2");
                assert_eq!(request.kind, "open_url");
                assert_eq!(request.value, "");
                break;
            }
            CoordWorkerUpstream::OmpEvents(_) => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
    close(&owner);
}

#[tokio::test]
async fn a_native_timeout_withdraws_its_own_browser_prompt() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "open-t",
        "device:a",
        "view-t",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    until(&mut receiver, &mut document, ui_frame).await;
    let origin = json!({"requestId": "open-t", "viewId": "view-t", "transportId": "open-t"});
    launcher.control(Control::Notify {
        method: "_omp/ui/host_service_request".into(),
        params: json!({
            "sessionId": "live-handle",
            "origin": origin.clone(),
            "serviceId": "svc-t",
            "kind": "copy_text",
            "value": "timed out payload",
        }),
    });
    let CoordWorkerUpstream::OmpHostServiceRequest(request) = next_upstream(&mut receiver).await
    else {
        panic!("host service request frame");
    };
    assert!(!request.cancelled);
    launcher.control(Control::Notify {
        method: "_omp/ui/host_service_cancel".into(),
        params: json!({
            "sessionId": "live-handle",
            "origin": origin,
            "serviceId": "svc-t",
            "kind": "copy_text",
        }),
    });
    loop {
        match next_upstream(&mut receiver).await {
            CoordWorkerUpstream::OmpHostServiceRequest(cancelled) => {
                assert!(cancelled.cancelled);
                assert_eq!(cancelled.service_id, "svc-t");
                assert_eq!(cancelled.kind, "copy_text");
                assert_eq!(cancelled.value, "");
                break;
            }
            CoordWorkerUpstream::OmpEvents(_) => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }
    owner.ui(view_action(
        "barrier",
        "device:a",
        "view-t",
        "open-t",
        "input",
        json!({"data":"barrier"}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    close(&owner);
}
