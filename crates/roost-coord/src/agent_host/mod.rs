//! Runtime state shared by the coordinator's built-in agent host features.
//!
//! The listener and worker-link dispatcher use the same tunnel registry so a
//! pipe WebSocket can be paired with exactly one authenticated worker link.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::ws::Message;
use tokio::sync::mpsc;

use crate::coord_core::worker_handle::WorkerHandle;

/// The coordinator's built-in agent-host runtime.
#[derive(Debug, Default)]
pub struct AgentHostRuntime {
    /// Active internal-pipe sessions, indexed by their random tunnel id.
    pub tunnels: AgentTunnelRegistry,
}

impl AgentHostRuntime {
    /// Build empty process-local agent host state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// Active internal WebSocket tunnels.
#[derive(Debug, Default)]
pub struct AgentTunnelRegistry {
    tunnels: Mutex<HashMap<String, TunnelEntry>>,
}

#[derive(Debug)]
struct TunnelEntry {
    worker_fp: String,
    worker: Arc<WorkerHandle>,
    pipe: mpsc::Sender<Message>,
}

impl AgentTunnelRegistry {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pipe and return its receiver for coordinator-to-pipe output.
    pub fn register(
        &self,
        tunnel_id: String,
        worker_fp: String,
        worker: Arc<WorkerHandle>,
    ) -> Option<mpsc::Receiver<Message>> {
        let (sender, receiver) = mpsc::channel(1024);
        let mut tunnels = self.tunnels.lock().ok()?;
        if tunnels.contains_key(&tunnel_id) {
            return None;
        }
        tunnels.insert(
            tunnel_id,
            TunnelEntry {
                worker_fp,
                worker,
                pipe: sender,
            },
        );
        Some(receiver)
    }

    /// Deliver a worker output only to its pipe; overflow retires that tunnel.
    pub fn send_from_worker(&self, worker_fp: &str, tunnel_id: &str, message: Message) -> bool {
        let sender = self.tunnels.lock().ok().and_then(|tunnels| {
            tunnels
                .get(tunnel_id)
                .filter(|entry| entry.worker_fp == worker_fp && entry.worker.is_routable())
                .map(|entry| entry.pipe.clone())
        });
        let Some(sender) = sender else { return false };
        if sender.try_send(message).is_ok() {
            return true;
        }
        if let Ok(mut tunnels) = self.tunnels.lock()
            && tunnels
                .get(tunnel_id)
                .is_some_and(|entry| entry.worker_fp == worker_fp)
        {
            tunnels.remove(tunnel_id);
        }
        false
    }

    /// Retire tunnels owned by one exact socket generation and tell each pipe why.
    fn retire_generation(&self, worker: &Arc<WorkerHandle>) {
        if let Ok(mut tunnels) = self.tunnels.lock() {
            tunnels.retain(|_, entry| {
                let is_generation = Arc::ptr_eq(&entry.worker, worker);
                if is_generation {
                    let _ =
                        entry
                            .pipe
                            .try_send(Message::Close(Some(axum::extract::ws::CloseFrame {
                                code: 1011,
                                reason: "worker link lost".into(),
                            })));
                }
                !is_generation
            });
        }
    }

    /// Remove one tunnel once its pipe socket closes.
    pub fn remove(&self, tunnel_id: &str) -> Option<Arc<WorkerHandle>> {
        self.tunnels
            .lock()
            .ok()?
            .remove(tunnel_id)
            .map(|entry| entry.worker)
    }
}

impl crate::coord_core::worker_lifecycle::WorkerLifecycleObserver for AgentHostRuntime {
    fn acknowledge_capabilities(
        &self,
        advertised: &std::collections::BTreeSet<String>,
    ) -> Vec<&'static str> {
        let capability = roost_protocol::versioning::CAPABILITY_AGENT_TOOL_TUNNEL_V1;
        advertised
            .contains(capability)
            .then_some(capability)
            .into_iter()
            .collect()
    }
    fn on_superseded(&self, superseded: &Arc<WorkerHandle>) {
        self.tunnels.retire_generation(superseded);
    }

    fn on_closed(
        &self,
        handle: &Arc<WorkerHandle>,
        _end: crate::coord_core::worker_lifecycle::LinkEnd,
    ) {
        self.tunnels.retire_generation(handle);
    }
}
