//! The bounded in-memory routes pairing worker tunnel frames with host pipes.
//!
//! `AgentHostRuntime` owns one registry and retires entries when the worker link
//! generation that opened them is superseded or closed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::ws::Message;
use tokio::sync::mpsc;

use crate::coord_core::worker_handle::WorkerHandle;

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
    pub(super) fn retire_generation(&self, worker: &Arc<WorkerHandle>) {
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
