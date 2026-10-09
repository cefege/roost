//! Worker-side process owner for opaque agent tool tunnels.
//!
//! Downstream tunnel frames are routed here by `runtime::downstream`; child
//! stdin/stdout are relayed back through the worker link's fenced uplink.

mod cache;
mod process;
mod support;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use roost_proto::{
    DAgentTunnelClose, DAgentTunnelDaemonChunk, DAgentTunnelInput, DAgentTunnelOpen,
};
use roost_protocol::wire::coord_worker::AgentTunnelState;
use tokio::process::Child;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use self::support::{current_platform, digest, state_frame, valid_sha256};
use crate::uplink::Uplink;
const MAX_DAEMON_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug)]
struct Tunnel {
    child: Child,
    input_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    input_task: JoinHandle<()>,
    stdout_task: JoinHandle<()>,
    relay_task: JoinHandle<()>,
    output_overflow: Arc<AtomicBool>,
    stderr_task: JoinHandle<()>,
}

#[derive(Debug)]
struct Upload {
    bytes: Vec<u8>,
    args: Vec<String>,
    platform: String,
    expected_sha: String,
}

#[derive(Debug, Default)]
struct State {
    tunnels: BTreeMap<String, Tunnel>,
    uploads: BTreeMap<String, Upload>,
}

/// Owns live tool processes and in-progress daemon transfers.
#[derive(Clone, Debug)]
pub struct AgentTunnelOwner {
    state: Arc<Mutex<State>>,
    cache: PathBuf,
    uplink: Uplink,
}

impl crate::link_ports::AgentTunnelPort for AgentTunnelOwner {
    fn open(&self, request: DAgentTunnelOpen) {
        let owner = self.clone();
        tokio::spawn(async move {
            AgentTunnelOwner::open(&owner, request).await;
        });
    }
    fn input(&self, request: DAgentTunnelInput) {
        let owner = self.clone();
        tokio::spawn(async move {
            AgentTunnelOwner::input(&owner, request).await;
        });
    }
    fn daemon_chunk(&self, request: DAgentTunnelDaemonChunk) {
        let owner = self.clone();
        tokio::spawn(async move {
            AgentTunnelOwner::daemon_chunk(&owner, request).await;
        });
    }
    fn close(&self, request: DAgentTunnelClose) {
        let owner = self.clone();
        tokio::spawn(async move {
            AgentTunnelOwner::close(&owner, request).await;
        });
    }
    fn close_all(&self) {
        let owner = self.clone();
        tokio::spawn(async move {
            AgentTunnelOwner::close_all(&owner).await;
        });
    }
}

impl AgentTunnelOwner {
    pub fn new(cache: PathBuf, uplink: Uplink) -> Self {
        Self {
            state: Arc::new(Mutex::new(State::default())),
            cache,
            uplink,
        }
    }

    pub async fn open(&self, request: DAgentTunnelOpen) {
        tracing::info!(tunnel_id = %request.tunnel_id, "agent tunnel opened");
        let args = match roost_protocol::wire::agent_chat::validated_daemon_args(&request.args) {
            Ok(args) => args.to_vec(),
            Err(_) => {
                self.closed(&request.tunnel_id, "invalid daemon args", -1);
                return;
            }
        };
        let Some(platform) = current_platform() else {
            self.closed(&request.tunnel_id, "unsupported platform", -1);
            return;
        };
        let Some(expected_sha) = request
            .daemon_sha256
            .get(&platform)
            .cloned()
            .filter(|sha| valid_sha256(sha))
        else {
            self.closed(&request.tunnel_id, "unsupported platform", -1);
            return;
        };
        match tokio::fs::read(self.cached_daemon_path(&platform, &expected_sha)).await {
            Ok(bytes) if digest(&bytes) == expected_sha => {
                self.spawn(request.tunnel_id, args, platform, expected_sha)
                    .await;
            }
            _ => {
                let mut state = self.state.lock().await;
                state.uploads.insert(
                    request.tunnel_id.clone(),
                    Upload {
                        bytes: Vec::new(),
                        args,
                        platform: platform.clone(),
                        expected_sha,
                    },
                );
                drop(state);
                tracing::info!(tunnel_id = %request.tunnel_id, %platform, "agent tunnel requested daemon upload");
                self.uplink.send(state_frame(
                    &request.tunnel_id,
                    AgentTunnelState::NeedDaemon,
                    &platform,
                    0,
                    "",
                ));
            }
        }
    }

    pub async fn daemon_chunk(&self, chunk: DAgentTunnelDaemonChunk) {
        let upload = {
            let mut state = self.state.lock().await;
            let oversized = state.uploads.get(&chunk.tunnel_id).is_some_and(|upload| {
                upload.bytes.len().saturating_add(chunk.data.len()) > MAX_DAEMON_BYTES
            });
            if oversized {
                state.uploads.remove(&chunk.tunnel_id);
                drop(state);
                self.closed(&chunk.tunnel_id, "daemon exceeds size limit", -1);
                return;
            }
            let Some(upload) = state.uploads.get_mut(&chunk.tunnel_id) else {
                return;
            };
            upload.bytes.extend_from_slice(&chunk.data);
            if !chunk.last {
                return;
            }
            state.uploads.remove(&chunk.tunnel_id)
        };
        let Some(upload) = upload else { return };
        if digest(&upload.bytes) != upload.expected_sha {
            self.closed(&chunk.tunnel_id, "daemon SHA mismatch", -1);
            return;
        }
        if self
            .persist_daemon(&upload.platform, &upload.expected_sha, &upload.bytes)
            .await
            .is_err()
        {
            self.closed(&chunk.tunnel_id, "daemon cache write failed", -1);
            return;
        }
        self.prune_daemon_cache(&upload.platform, &upload.expected_sha)
            .await;
        tracing::info!(tunnel_id = %chunk.tunnel_id, platform = %upload.platform, "agent tunnel daemon cached and verified");
        self.spawn(
            chunk.tunnel_id,
            upload.args,
            upload.platform,
            upload.expected_sha,
        )
        .await;
    }

    pub async fn input(&self, input: DAgentTunnelInput) {
        let full = {
            let state = self.state.lock().await;
            state
                .tunnels
                .get(&input.tunnel_id)
                .is_some_and(|tunnel| tunnel.input_tx.try_send(input.data).is_err())
        };
        if full {
            tracing::warn!(tunnel_id = %input.tunnel_id, "agent tunnel input queue is full; closing tunnel");
            self.close(DAgentTunnelClose {
                tunnel_id: input.tunnel_id,
                ..Default::default()
            })
            .await;
        }
    }

    pub async fn close(&self, request: DAgentTunnelClose) {
        let mut state = self.state.lock().await;
        let upload_removed = state.uploads.remove(&request.tunnel_id).is_some();
        let tunnel_removed = if let Some(mut tunnel) = state.tunnels.remove(&request.tunnel_id) {
            tunnel.input_task.abort();
            tunnel.stdout_task.abort();
            tunnel.stderr_task.abort();
            tunnel.relay_task.abort();
            let _ = tunnel.child.start_kill();
            true
        } else {
            false
        };
        if upload_removed || tunnel_removed {
            tracing::info!(tunnel_id = %request.tunnel_id, "agent tunnel closed");
        }
    }

    pub async fn close_all(&self) {
        let ids = {
            let state = self.state.lock().await;
            state
                .tunnels
                .keys()
                .chain(state.uploads.keys())
                .cloned()
                .collect::<Vec<_>>()
        };
        for tunnel_id in ids {
            self.close(DAgentTunnelClose {
                tunnel_id,
                ..Default::default()
            })
            .await;
        }
    }
}
