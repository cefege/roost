//! The terminal-image RPC relays a content key and returns decoded PNG bytes.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "terminal_screen_support/mod.rs"]
mod support;

use base64::Engine;
use roost_coord::terminal_screen::image_rpc::handle_sessions_get_terminal_image;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::control::ClientControlFrame;
use support::{Harness, WORKER_FP, frame_of, wait_for_frame};

#[tokio::test]
async fn image_request_reaches_worker_and_returns_png_bytes() {
    let harness = Harness::new("image-rpc").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();
    let handle = tokio::spawn(async move {
        handle_sessions_get_terminal_image(
            &core,
            &caller,
            roost_proto::SessionsGetTerminalImageRequest {
                session_id,
                image_key: 987654,
                ..Default::default()
            },
        )
        .await
    });

    let sent = wait_for_frame(&harness, 1).await;
    let (browser_id, frame, viewer_id, request_id) = frame_of(&sent);
    assert_eq!(browser_id, "browser-fp");
    assert_eq!(viewer_id, "browser-fp");
    match frame {
        ClientControlFrame::GetTerminalImage {
            session_id,
            image_key,
            ..
        } => {
            assert_eq!(session_id.as_str(), harness.session_id);
            assert_eq!(*image_key, 987654);
        }
        other => panic!("an image fetch is a get-terminal-image frame, got {other:?}"),
    }

    let png = b"\x89PNG\r\n\x1a\nimage";
    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    assert!(harness.core.services.scrollback.pending().resolve(
        request_id,
        serde_json::json!({"png": base64::engine::general_purpose::STANDARD.encode(png)}),
        Some(worker.as_str()),
    ));
    let response = handle
        .await
        .expect("the handler task finished")
        .expect("the image is served");
    assert_eq!(response.body.png, png);
}

#[tokio::test]
async fn missing_image_reply_becomes_not_found() {
    let harness = Harness::new("image-rpc-missing").await;
    let core = harness.core.clone();
    let caller = harness.caller();
    let session_id = harness.session_id.clone();
    let handle = tokio::spawn(async move {
        handle_sessions_get_terminal_image(
            &core,
            &caller,
            roost_proto::SessionsGetTerminalImageRequest {
                session_id,
                image_key: 987654,
                ..Default::default()
            },
        )
        .await
    });
    let sent = wait_for_frame(&harness, 1).await;
    let (_, _, _, request_id) = frame_of(&sent);
    let worker = WorkerFp::try_from(WORKER_FP).unwrap();
    assert!(harness.core.services.scrollback.pending().resolve(
        request_id,
        serde_json::json!({"png": null}),
        Some(worker.as_str()),
    ));
    let error = handle
        .await
        .expect("the handler task finished")
        .expect_err("a missing image is not served");
    assert_eq!(error.code, connectrpc::ErrorCode::NotFound);
    assert_eq!(error.message.as_deref(), Some("terminal image not found"));
}
