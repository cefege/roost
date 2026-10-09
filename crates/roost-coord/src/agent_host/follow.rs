//! Reconnectable NDJSON follower for durable agent-host events.
//!
//! A single task owns the HTTP stream; each complete line updates the shared
//! cache before publishing corresponding current-value Sync frames.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::services::CoordServices;

const MAX_LINE_BYTES: usize = 32 * 1024 * 1024;

/// Task owner whose stop waits for the follower to exit.
#[derive(Debug)]
pub struct FollowerHandle {
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl FollowerHandle {
    /// Signal shutdown and wait for the event stream to close.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
    }
}

/// Spawn the host stream follower; an unconfigured coordinator gets a dormant task.
pub fn spawn_follower(services: Arc<CoordServices>) -> FollowerHandle {
    let (stop, mut stopped) = watch::channel(false);
    let task = tokio::spawn(async move {
        let Ok(config) = services.boot.require_config() else {
            return;
        };
        let (Some(base), Some(secret)) = (&config.agent_host_url, &config.agent_host_secret) else {
            return;
        };
        let client = super::client::AgentHostClient::new(base.clone(), secret.clone());
        let mut backoff = Duration::from_secs(1);
        loop {
            if *stopped.borrow() {
                break;
            }
            match client.events().await {
                Ok(mut response) if response.status().is_success() => {
                    tracing::info!(
                        event = "agent_host.connected",
                        "agent host event stream connected"
                    );
                    publish_connected(&services, true);
                    backoff = Duration::from_secs(1);
                    let mut pending = Vec::new();
                    let mut last_line = tokio::time::Instant::now();
                    loop {
                        tokio::select! {
                            changed = stopped.changed() => { if changed.is_err() || *stopped.borrow() { return; } }
                            chunk = response.chunk() => match chunk {
                                Ok(Some(bytes)) => {
                                    pending.extend_from_slice(&bytes);
                                    if pending.len() > MAX_LINE_BYTES && !pending.contains(&b'\n') { break; }
                                    while let Some(newline) = pending.iter().position(|b| *b == b'\n') {
                                        let line: Vec<u8> = pending.drain(..=newline).collect();
                                        if line.len() > MAX_LINE_BYTES { pending.clear(); break; }
                                        let line = line.strip_suffix(b"\n").unwrap_or(&line);
                                        if let Ok(parsed) = serde_json::from_slice::<roost_protocol::wire::agent_chat::HostStreamLine>(line) {
                                            publish_line(&services, &parsed);
                                            last_line = tokio::time::Instant::now();
                                        }
                                    }
                                    if pending.len() > MAX_LINE_BYTES { break; }
                                }
                                Ok(None) | Err(_) => break,
                            },
                            () = tokio::time::sleep_until(last_line + Duration::from_secs(45)) => break,
                        }
                    }
                    tracing::info!(
                        event = "agent_host.disconnected",
                        "agent host event stream disconnected"
                    );
                    publish_connected(&services, false);
                }
                _ => {
                    publish_connected(&services, false);
                }
            }
            tokio::select! {
                changed = stopped.changed() => { if changed.is_err() || *stopped.borrow() { break; } }
                () = tokio::time::sleep(backoff) => {}
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    });
    FollowerHandle { stop, task }
}

fn publish_connected(services: &CoordServices, connected: bool) {
    let update = services
        .agent_host
        .cache
        .lock()
        .ok()
        .and_then(|mut cache| cache.set_connected(connected));
    if let Some(update) = update {
        services.agent_host.publish(services, update);
    }
}

fn publish_line(services: &CoordServices, line: &roost_protocol::wire::agent_chat::HostStreamLine) {
    let updates = services
        .agent_host
        .cache
        .lock()
        .ok()
        .map(|mut cache| cache.apply_line(line))
        .unwrap_or_default();
    for update in updates {
        services.agent_host.publish(services, update);
    }
}
