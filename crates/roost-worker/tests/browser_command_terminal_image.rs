//! Browser image retrieval answers from the session core's retained pixels.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod browser_command_support;
mod terminal_stream_support;

use base64::Engine as _;
use roost_protocol::wire::brand::SessionId;
use roost_term::{RioCore, TerminalCore};
use roost_worker::browser_commands::terminal_image::TerminalImages;
use roost_worker::session::terminal_images::SessionTerminalImages;
use serde_json::json;
use terminal_stream_support::{COLS, Harness, ROWS, SESSION};

#[tokio::test]
async fn browser_command_fetches_retained_png_and_null_for_missing_key() {
    let mut core = RioCore::new(COLS, ROWS);
    core.write_raw(b"\x1b_Ga=T,f=32,s=2,v=2,c=4,r=2;/wAA//8AAP//AAD//wAA//==\x1b\\");
    let session = Harness::new(core);
    let placements = session
        .table
        .with_record(&terminal_stream_support::session_id(), |record| {
            record.terminal_core.image_placements()
        })
        .expect("the harness session exists");
    let image_key = placements
        .first()
        .map(|image| image.image_key)
        .expect("kitty image is retained");
    let images = std::sync::Arc::new(SessionTerminalImages::new(session.table.clone()));
    let session_id = SessionId::try_from(SESSION.to_owned()).expect("session id");
    let png = images
        .png(session_id.clone(), image_key)
        .await
        .expect("session exists")
        .expect("retained PNG");
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

    let mut support = browser_command_support::harness();
    support.deps.images = images.clone();
    let (_, mut request) = browser_command_support::frames::frame(
        "get-terminal-image",
        &[
            ("request_id", json!("image")),
            ("image_key", json!(image_key)),
        ],
    );
    request["session_id"] = json!(SESSION);
    let command = browser_command_support::command(request);
    let reply = browser_command_support::only(
        roost_worker::browser_commands::dispatch(&command, &support.deps).await,
    );
    let encoded = reply
        .data()
        .and_then(|data| data["png"].as_str())
        .expect("PNG reply");
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("base64 PNG");
    assert!(decoded.starts_with(b"\x89PNG\r\n\x1a\n"));

    let missing_key = images
        .png(session_id.clone(), u64::MAX)
        .await
        .expect("session exists");
    assert!(missing_key.is_none());
    let unknown_session =
        SessionId::try_from("11111111-2222-4333-8444-666666666666").expect("session id");
    assert!(images.png(unknown_session, image_key).await.is_err());
    let missing = roost_protocol::wire::control::ClientControlFrame::GetTerminalImage {
        request_id: "missing".to_owned(),
        session_id,
        image_key: u64::MAX,
        trace_id: None,
    };
    let absent =
        roost_worker::browser_commands::Command::new("browser", "viewer", "req-1", missing);
    let reply = browser_command_support::only(
        roost_worker::browser_commands::dispatch(&absent, &support.deps).await,
    );
    assert_eq!(reply.data(), Some(&json!({ "png": null })));
}

#[test]
fn local_image_wire_response_echoes_request_identity() {
    let response = roost_proto::LocalImageResponse {
        request_id: "image".to_owned(),
        session_id: SESSION.to_owned(),
        image_key: 42,
        error: "terminal image not found".to_owned(),
        ..Default::default()
    };
    assert_eq!(response.request_id, "image");
    assert_eq!(response.session_id, SESSION);
    assert_eq!(response.image_key, 42);
    assert_eq!(response.error, "terminal image not found");
}
