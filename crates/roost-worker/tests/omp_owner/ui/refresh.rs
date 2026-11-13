//! Provider and native presentation recovery retain session-owned identities.
//! Exercises the real owner across session reload and view lease renewal.
//! The last negotiated viewport survives without resetting other live views.

use super::*;
use roost_proto::DOmpOpen;

#[tokio::test]
async fn provider_refresh_waits_for_native_turn_and_recovers_the_actual_resume_identity() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    let mut document = OmpUiDocument::default();
    owner.ui(open(
        "native-generation",
        "device:a",
        "view-a",
        json!({
            "cols": 80, "rows": 24, "dark": true, "reduceMotion": false
        }),
    ));
    peer_request(&mut requests).await;
    until(&mut receiver, &mut document, ui_frame).await;
    launcher.control(Control::State {
        streaming: true,
        resume_id: "selected-history".into(),
    });
    until(&mut receiver, &mut document, |event| {
        matches!(event, OmpWorkerEvent::TurnStarted { .. })
    })
    .await;
    owner.open(DOmpOpen {
        conversation_id: "c1".into(),
        cwd: "/work".into(),
        acp_session_id: "outdated-live-handle".into(),
        ..Default::default()
    });
    owner.ui(view_action(
        "busy-barrier",
        "device:a",
        "view-a",
        "native-generation",
        "input",
        json!({"data":"barrier"}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    launcher.control(Control::State {
        streaming: true,
        resume_id: "selected-history".into(),
    });
    until(&mut receiver, &mut document, |event| {
        matches!(
            event,
            OmpWorkerEvent::UiState {
                streaming: true,
                ..
            }
        )
    })
    .await;
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    launcher.control(Control::State {
        streaming: false,
        resume_id: "selected-history".into(),
    });
    let loaded = peer_request(&mut requests).await;
    assert_eq!(loaded.method, "load");
    assert_eq!(loaded.params["resumeId"], "selected-history");
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    close(&owner);
}

#[tokio::test]
async fn view_renewals_preserve_the_last_negotiated_viewport_across_child_replacement() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink, Arc::new(Default::default()));
    let mut document = OmpUiDocument::default();
    let first = json!({
        "cols":80, "rows":24, "dark":true, "reduceMotion":false,
        "kinds":["col","text"], "features":[]
    });
    owner.ui(open(
        "viewport-generation",
        "device:a",
        "view-a",
        first.clone(),
    ));
    peer_request(&mut requests).await;
    until(&mut receiver, &mut document, ui_frame).await;
    owner.ui(open(
        "larger-view",
        "device:b",
        "view-b",
        json!({
            "cols":100, "rows":30, "dark":true, "reduceMotion":false,
            "kinds":["col","text","picker"], "features":["scroll"]
        }),
    ));
    let negotiated = peer_request(&mut requests).await;
    assert_eq!(negotiated.method, "_omp/ui/open");
    assert_eq!(negotiated.params["transportId"], "viewport-generation");
    let delivered = until(&mut receiver, &mut document, ui_frame).await;
    assert!(reset_transports(&delivered).is_empty());
    owner.ui(view_action(
        "height-change",
        "device:b",
        "view-b",
        "viewport-generation",
        "event",
        json!({"event":{"ev":"resize","cols":100,"rows":45}}),
    ));
    peer_request(&mut requests).await;
    owner.ui(view_action(
        "width-only-change",
        "device:b",
        "view-b",
        "viewport-generation",
        "event",
        json!({"event":{"ev":"resize","cols":110}}),
    ));
    peer_request(&mut requests).await;
    owner.ui(open("renew-first", "device:a", "view-a", first));
    owner.ui(open(
        "renew-second",
        "device:b",
        "view-b",
        json!({
            "cols":110, "rows":45, "dark":true, "reduceMotion":false,
            "kinds":["col","text","picker"], "features":["scroll"]
        }),
    ));
    owner.ui(view_action(
        "close-second",
        "device:b",
        "view-b",
        "viewport-generation",
        "close",
        json!({}),
    ));
    owner.ui(view_action(
        "surviving-view",
        "device:a",
        "view-a",
        "viewport-generation",
        "input",
        json!({"data":"barrier"}),
    ));
    assert_eq!(peer_request(&mut requests).await.method, "_omp/ui/input");
    owner.open(DOmpOpen {
        conversation_id: "c1".into(),
        cwd: "/work".into(),
        acp_session_id: "live-handle".into(),
        ..Default::default()
    });
    assert_eq!(peer_request(&mut requests).await.method, "load");
    let recovered = peer_request(&mut requests).await;
    assert_eq!(recovered.method, "_omp/ui/open");
    assert_eq!(recovered.params["cols"], 110);
    assert_eq!(recovered.params["rows"], 45);
    assert_eq!(recovered.params["kinds"], json!(["col", "text", "picker"]));
    assert_eq!(recovered.params["features"], json!(["scroll"]));
    assert_ne!(recovered.params["transportId"], "viewport-generation");
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    close(&owner);
}
