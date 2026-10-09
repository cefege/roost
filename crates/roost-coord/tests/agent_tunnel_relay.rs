//! The internal agent tunnel over the coordinator router and worker registry.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
#[path = "middleware_support/mod.rs"]
mod middleware_support;
mod ws_client_support;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message as ServerMessage};
use futures_util::SinkExt;
use middleware_support::{FixtureConfig, ListenerFixture};
use roost_coord::coord_core::worker_handle::WorkerHandle;
use roost_proto::{DAgentTunnelDaemonChunk, DAgentTunnelInput, DAgentTunnelOpen};
use roost_protocol::versioning::CAPABILITY_AGENT_TOOL_TUNNEL_V1;
use roost_protocol::wire::WorkerFp;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use ws_client_support::{WsClient, next_frame};

const WORKER_FP: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const NO_CAPABILITY_FP: &str = "fedcba9876543210fedcba9876543210fedcba9876543210fedcba9876543210";
const HOST_SECRET: &str = "a-32-byte-agent-host-secret-value!";

fn register_worker(
    fixture: &ListenerFixture,
    fingerprint: &str,
    capabilities: BTreeSet<String>,
) -> (Arc<WorkerHandle>, Arc<Mutex<Vec<CoordWorkerDownstream>>>) {
    let frames = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&frames);
    let handle = Arc::new(WorkerHandle::new(
        WorkerFp::try_from(fingerprint).expect("worker fingerprint"),
        Some("test-epoch".to_owned()),
        "test-generation".to_owned(),
        capabilities,
        Arc::new(move |frame| {
            captured.lock().expect("frame capture lock").push(frame);
            1
        }),
    ));
    fixture.services.workers.insert(Arc::clone(&handle));
    handle.mark_ready();
    (handle, frames)
}

async fn dial_agent_pipe(
    fixture: &ListenerFixture,
    fingerprint: &str,
    bearer: Option<&str>,
) -> Result<WsClient, u16> {
    let mut request = format!(
        "ws://{}/internal/agent-env/{fingerprint}",
        fixture.own_host()
    )
    .into_client_request()
    .expect("WebSocket request");
    if let Some(bearer) = bearer {
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {bearer}")
                .parse()
                .expect("authorization header"),
        );
    }
    match tokio_tungstenite::connect_async(request).await {
        Ok((socket, _)) => Ok(socket),
        Err(WsError::Http(response)) => Err(response.status().as_u16()),
        Err(error) => panic!("WebSocket dial failed: {error}"),
    }
}

async fn worker_frame(
    frames: &Arc<Mutex<Vec<CoordWorkerDownstream>>>,
    index: usize,
) -> CoordWorkerDownstream {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(frame) = frames
                .lock()
                .expect("frame capture lock")
                .get(index)
                .cloned()
            {
                return frame;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("worker received the tunnel frame")
}

async fn configured_fixture(label: &str) -> ListenerFixture {
    ListenerFixture::start(
        label,
        FixtureConfig {
            agent_host_secret: Some(HOST_SECRET.to_owned()),
            ..FixtureConfig::default()
        },
    )
    .await
}

#[tokio::test]
async fn internal_tunnel_refuses_unconfigured_bad_secret_and_missing_capability() {
    let unconfigured =
        ListenerFixture::start("agent-tunnel-unconfigured", FixtureConfig::default()).await;
    assert!(matches!(
        dial_agent_pipe(&unconfigured, WORKER_FP, Some(HOST_SECRET)).await,
        Err(404)
    ));
    let fixture = configured_fixture("agent-tunnel-admission").await;
    assert!(matches!(
        dial_agent_pipe(&fixture, WORKER_FP, Some("wrong")).await,
        Err(401)
    ));

    let (_worker, _frames) = register_worker(&fixture, NO_CAPABILITY_FP, BTreeSet::new());
    let mut socket = dial_agent_pipe(&fixture, NO_CAPABILITY_FP, Some(HOST_SECRET))
        .await
        .expect("authenticated pipe upgrades before worker admission");
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "open",
                "args": ["serve", "--token", "0123456789abcdef0123456789abcdef"],
                "daemons": {}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("open request");
    let Some(Message::Close(Some(close))) = next_frame(&mut socket, Duration::from_secs(2)).await
    else {
        panic!("a worker without the tunnel capability is closed");
    };
    assert_eq!(u16::from(close.code), 1011);
    assert_eq!(close.reason, "missing capability");
}

#[tokio::test]
async fn tunnel_relays_open_daemon_input_output_and_worker_exit() {
    use sha2::Digest as _;

    let fixture = configured_fixture("agent-tunnel-relay").await;
    let (worker, frames) = register_worker(
        &fixture,
        WORKER_FP,
        BTreeSet::from([CAPABILITY_AGENT_TOOL_TUNNEL_V1.to_owned()]),
    );
    let mut socket = dial_agent_pipe(&fixture, WORKER_FP, Some(HOST_SECRET))
        .await
        .expect("authenticated pipe upgrades");
    let daemon = b"daemon-bytes";
    let daemon_sha = hex::encode(sha2::Sha256::digest(daemon));
    socket
        .send(Message::Text(
            serde_json::json!({
                "type": "open",
                "args": ["serve", "--token", "0123456789abcdef0123456789abcdef"],
                "daemons": {"linux-x64": daemon_sha}
            })
            .to_string()
            .into(),
        ))
        .await
        .expect("open request");
    let CoordWorkerDownstream::AgentTunnelOpen(DAgentTunnelOpen {
        tunnel_id, args, ..
    }) = worker_frame(&frames, 0).await
    else {
        panic!("the coordinator opens a worker tunnel");
    };
    assert_eq!(args[0], "serve");
    assert!(
        fixture.services.agent_host.tunnels.send_from_worker(
            worker.worker_fp.as_str(),
            &tunnel_id,
            ServerMessage::Text(
                serde_json::json!({"type":"need_daemon", "platform":"linux-x64"})
                    .to_string()
                    .into()
            ),
        )
    );
    assert!(matches!(
        next_frame(&mut socket, Duration::from_secs(2)).await,
        Some(Message::Text(text)) if text.contains("need_daemon")
    ));
    socket
        .send(Message::Text(
            serde_json::json!({"type":"daemon_chunk_begin", "size":daemon.len()})
                .to_string()
                .into(),
        ))
        .await
        .expect("daemon upload start");
    socket
        .send(Message::Binary(daemon.to_vec().into()))
        .await
        .expect("daemon bytes");
    socket
        .send(Message::Text("{\"type\":\"daemon_end\"}".into()))
        .await
        .expect("daemon upload end");
    let mut saw_last = false;
    for index in 1..=2 {
        match worker_frame(&frames, index).await {
            CoordWorkerDownstream::AgentTunnelDaemonChunk(DAgentTunnelDaemonChunk {
                data,
                last,
                ..
            }) => {
                saw_last |= last;
                if !last {
                    assert_eq!(data, daemon);
                }
            }
            frame => panic!("daemon upload emitted an unexpected worker frame: {frame:?}"),
        }
    }
    assert!(saw_last, "the daemon upload is explicitly finalized");

    socket
        .send(Message::Binary(b"stdin".to_vec().into()))
        .await
        .expect("stdin");
    assert!(matches!(worker_frame(&frames, 3).await,
        CoordWorkerDownstream::AgentTunnelInput(DAgentTunnelInput { data, .. }) if data == b"stdin"));
    assert!(fixture.services.agent_host.tunnels.send_from_worker(
        worker.worker_fp.as_str(),
        &tunnel_id,
        ServerMessage::Binary(b"stdout".to_vec().into()),
    ));
    assert!(
        matches!(next_frame(&mut socket, Duration::from_secs(2)).await,
        Some(Message::Binary(data)) if data.as_ref() == b"stdout")
    );
    assert!(fixture.services.agent_host.tunnels.send_from_worker(
        worker.worker_fp.as_str(),
        &tunnel_id,
        ServerMessage::Close(Some(CloseFrame {
            code: 1000,
            reason: "exit 0".into()
        })),
    ));
    fixture.services.agent_host.tunnels.remove(&tunnel_id);
    assert!(
        matches!(next_frame(&mut socket, Duration::from_secs(2)).await,
        Some(Message::Close(Some(close))) if u16::from(close.code) == 1000 && close.reason == "exit 0")
    );
}
