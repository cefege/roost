//! Provider refresh waits for the native turn and reloads the selected history identity.

use super::*;
use roost_proto::DOmpOpen;

#[tokio::test]
async fn provider_refresh_waits_for_native_turn_and_recovers_the_actual_resume_identity() {
    let (launcher, mut requests) = UiLauncher::new();
    let launcher = Arc::new(launcher);
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
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
