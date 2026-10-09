//! The loopback-only WebSocket carrying the agent host's opaque worker daemon.
//!
//! The route authenticates before upgrading, then forwards bytes through the
//! worker link without interpreting the daemon protocol.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use roost_proto::{
    DAgentTunnelClose, DAgentTunnelDaemonChunk, DAgentTunnelInput, DAgentTunnelOpen,
};
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde::Deserialize;

use crate::coord_core::worker_handle::WorkerHandle;
use crate::http::listener::ListenerState;

const MAX_DAEMON_BYTES: usize = 64 * 1024 * 1024;
const AGENT_TUNNEL_CHUNK_BYTES: usize = 256 * 1024;

#[derive(Deserialize)]
struct PipeOpen {
    #[serde(rename = "type")]
    kind: String,
    args: Vec<String>,
    daemons: std::collections::BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct DaemonBegin {
    #[serde(rename = "type")]
    kind: String,
    size: usize,
}

#[derive(Deserialize)]
struct PipeNotification {
    #[serde(rename = "type")]
    kind: String,
    platform: String,
}

/// Admit the private pipe only from loopback with the configured shared secret.
pub async fn agent_env_upgrade(
    State(state): State<Arc<ListenerState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(worker_fp): Path<String>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let Some(secret) = state.service.config.agent_host_secret.as_deref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !peer.ip().is_loopback() {
        return StatusCode::FORBIDDEN.into_response();
    }
    if !authorized(&headers, secret) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(identity) = roost_protocol::wire::WorkerFp::try_from(worker_fp.as_str()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(worker) = state.services.workers.current_routable(&identity) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let services = Arc::clone(&state.services);
    upgrade
        .max_message_size(crate::http::listener::MAX_WEBSOCKET_PAYLOAD_BYTES)
        .on_upgrade(move |socket| run_pipe(socket, services, worker_fp, worker))
}

fn authorized(headers: &HeaderMap, secret: &str) -> bool {
    use sha2::Digest as _;
    let Some(value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Some(presented) = value.strip_prefix("Bearer ") else {
        return false;
    };
    let expected = sha2::Sha256::digest(secret.as_bytes());
    let actual = sha2::Sha256::digest(presented.as_bytes());
    actual
        .iter()
        .zip(expected.iter())
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

async fn run_pipe(
    mut socket: WebSocket,
    services: Arc<crate::services::CoordServices>,
    worker_fp: String,
    worker: Arc<WorkerHandle>,
) {
    use sha2::Digest as _;
    let Some(Ok(Message::Text(first))) = socket.next().await else {
        close(&mut socket, 1008, "invalid args").await;
        return;
    };
    let Ok(open) = serde_json::from_str::<PipeOpen>(&first) else {
        close(&mut socket, 1008, "invalid args").await;
        return;
    };
    if open.kind != "open" {
        close(&mut socket, 1008, "invalid args").await;
        return;
    }
    let validated_args = match roost_protocol::wire::agent_chat::validated_daemon_args(&open.args) {
        Ok(args) => args,
        Err(_) => {
            close(&mut socket, 1008, "invalid args").await;
            return;
        }
    };
    if !worker
        .capabilities
        .contains(roost_protocol::versioning::CAPABILITY_AGENT_TOOL_TUNNEL_V1)
    {
        close(&mut socket, 1011, "missing capability").await;
        return;
    }
    let tunnel_id = match crate::coord_core::ids::draw::<16>() {
        Ok(bytes) => crate::coord_core::ids::render_v4(bytes),
        Err(error) => {
            tracing::error!(%error, "agent tunnel: no entropy for tunnel id");
            close(&mut socket, 1011, "tunnel id unavailable").await;
            return;
        }
    };
    let Some(mut output) = services.agent_host.tunnels.register(
        tunnel_id.clone(),
        worker_fp.clone(),
        Arc::clone(&worker),
    ) else {
        close(&mut socket, 1011, "tunnel registry unavailable").await;
        return;
    };
    let daemon_sha256 = open.daemons.clone();
    let open_frame = CoordWorkerDownstream::AgentTunnelOpen(DAgentTunnelOpen {
        tunnel_id: tunnel_id.clone(),
        args: validated_args.into(),
        daemon_sha256: daemon_sha256
            .iter()
            .map(|(platform, sha)| (platform.clone(), sha.clone()))
            .collect(),
        ..Default::default()
    });
    if worker.send(open_frame) == 0 {
        services.agent_host.tunnels.remove(&tunnel_id);
        close(&mut socket, 1011, "worker link lost").await;
        return;
    }
    let (mut sink, mut stream) = socket.split();
    let mut daemon_upload: Option<(usize, usize, sha2::Sha256)> = None;
    let mut needed_platform = None::<String>;
    'relay: loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    if let Ok(begin) = serde_json::from_str::<DaemonBegin>(&text) {
                        let valid_request = begin.kind == "daemon_chunk_begin"
                            && begin.size <= MAX_DAEMON_BYTES
                            && needed_platform.as_ref().is_some_and(|platform| {
                                daemon_sha256
                                    .get(platform)
                                    .is_some_and(|sha| valid_sha256(sha))
                            });
                        if !valid_request || daemon_upload.is_some() {
                            close_sink(&mut sink, 1011, "unsupported platform or sha mismatch").await;
                            break;
                        }
                        daemon_upload = Some((begin.size, 0, sha2::Sha256::new()));
                    } else if text == "{\"type\":\"daemon_end\"}" {
                        let Some((expected_size, received_size, hasher)) = daemon_upload.take() else {
                            close_sink(&mut sink, 1008, "unexpected daemon_end").await;
                            break;
                        };
                        let actual_sha256 = hex::encode(hasher.finalize());
                        let expected_sha256 = needed_platform.as_ref()
                            .and_then(|platform| daemon_sha256.get(platform));
                        if received_size != expected_size
                            || expected_sha256.is_none_or(|expected| expected != &actual_sha256)
                        {
                            close_sink(&mut sink, 1011, "sha mismatch").await;
                            break;
                        }
                        if worker.send(CoordWorkerDownstream::AgentTunnelDaemonChunk(
                            DAgentTunnelDaemonChunk {
                                tunnel_id: tunnel_id.clone(),
                                data: Vec::new(),
                                last: true,
                                ..Default::default()
                            }
                        )) == 0 {
                            close_sink(&mut sink, 1011, "worker link lost").await;
                            break;
                        }
                        needed_platform = None;
                    } else {
                        close_sink(&mut sink, 1008, "unexpected text message").await;
                        break;
                    }
                }
                Some(Ok(Message::Binary(data))) => {
                    if let Some((expected, received, hasher)) = &mut daemon_upload {
                        if received.saturating_add(data.len()) > *expected {
                            close_sink(&mut sink, 1008, "invalid daemon size").await;
                            break 'relay;
                        }
                        *received += data.len();
                        hasher.update(&data);
                        let mut failed = false;
                        for chunk in data.chunks(AGENT_TUNNEL_CHUNK_BYTES) {
                            if worker.send(CoordWorkerDownstream::AgentTunnelDaemonChunk(
                                DAgentTunnelDaemonChunk {
                                    tunnel_id: tunnel_id.clone(),
                                    data: chunk.to_vec(),
                                    last: false,
                                    ..Default::default()
                                }
                            )) == 0 {
                                close_sink(&mut sink, 1011, "worker link lost").await;
                                failed = true;
                                break;
                            }
                        }
                        if failed { break 'relay; }
                    } else {
                        for chunk in data.chunks(AGENT_TUNNEL_CHUNK_BYTES) {
                            if worker.send(CoordWorkerDownstream::AgentTunnelInput(DAgentTunnelInput {
                                tunnel_id: tunnel_id.clone(),
                                data: chunk.to_vec(),
                                ..Default::default()
                            })) == 0 {
                                close_sink(&mut sink, 1011, "worker link lost").await;
                                break 'relay;
                            }
                        }
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(Message::Ping(_))) | Some(Ok(Message::Pong(_))) => {}
                Some(Err(_)) => break,
            },
            outgoing = output.recv() => match outgoing {
                Some(message) => {
                    if let Message::Text(text) = &message
                        && let Ok(notification) = serde_json::from_str::<PipeNotification>(text)
                        && notification.kind == "need_daemon"
                    {
                        if daemon_sha256.get(&notification.platform).is_none_or(|sha| !valid_sha256(sha)) {
                            close_sink(&mut sink, 1011, "unsupported platform").await;
                            break;
                        }
                        needed_platform = Some(notification.platform);
                    }
                    if sink.send(message).await.is_err() {
                        break;
                    }
                }
                None => break,
            }
        }
    }
    services.agent_host.tunnels.remove(&tunnel_id);
    worker.send(CoordWorkerDownstream::AgentTunnelClose(DAgentTunnelClose {
        tunnel_id,
        ..Default::default()
    }));
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

async fn close(socket: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

async fn close_sink<S>(sink: &mut S, code: u16, reason: &'static str)
where
    S: futures_util::Sink<Message> + Unpin,
{
    let _ = sink
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}
