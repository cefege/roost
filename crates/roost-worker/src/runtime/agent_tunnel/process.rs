//! Agent daemon process startup, relay pumps, and exit state reporting.
//!
//! Owned by `AgentTunnelOwner`; all bytes cross the existing fenced worker
//! uplink, and bounded queues ensure one stuck pipe cannot consume unbounded memory.

use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::process::Command;

use super::support::{home_directory, pump, state_frame, write_input};
use super::{AgentTunnelOwner, Tunnel};
use roost_protocol::wire::coord_worker::{AgentTunnelState, CoordWorkerUpstream};

impl AgentTunnelOwner {
    pub(super) async fn spawn(
        &self,
        tunnel_id: String,
        args: Vec<String>,
        platform: String,
        sha256: String,
    ) {
        let path = self.cached_daemon_path(&platform, &sha256);
        let Some(home) = home_directory() else {
            self.closed(&tunnel_id, "worker home directory unavailable", -1);
            return;
        };
        let mut command = Command::new(path);
        command
            .current_dir(home)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let Ok(mut child) = command.spawn() else {
            self.closed(&tunnel_id, "daemon spawn failed", -1);
            return;
        };
        let Some(stdin) = child.stdin.take() else {
            self.closed(&tunnel_id, "daemon stdin unavailable", -1);
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            self.closed(&tunnel_id, "daemon stdout unavailable", -1);
            return;
        };
        let Some(stderr) = child.stderr.take() else {
            self.closed(&tunnel_id, "daemon stderr unavailable", -1);
            return;
        };
        let (input_tx, input_rx) = tokio::sync::mpsc::channel(256);
        let input_task = write_input(stdin, input_rx, tunnel_id.clone());
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(256);
        let output_uplink = self.uplink.clone();
        let relay_task = tokio::spawn(async move {
            while let Some(output) = output_rx.recv().await {
                output_uplink.send(CoordWorkerUpstream::AgentTunnelOutput(output));
            }
        });
        let output_overflow = Arc::new(AtomicBool::new(false));
        let stdout_task = pump(
            stdout,
            output_tx.clone(),
            tunnel_id.clone(),
            false,
            Arc::clone(&output_overflow),
        );
        let stderr_task = pump(
            stderr,
            output_tx,
            tunnel_id.clone(),
            true,
            Arc::clone(&output_overflow),
        );
        let mut state = self.state.lock().await;
        if let Some(mut old) = state.tunnels.insert(
            tunnel_id.clone(),
            Tunnel {
                child,
                input_tx,
                input_task,
                stdout_task,
                stderr_task,
                relay_task,
                output_overflow: Arc::clone(&output_overflow),
            },
        ) {
            old.input_task.abort();
            old.stdout_task.abort();
            old.stderr_task.abort();
            old.relay_task.abort();
            let _ = old.child.start_kill();
        }
        drop(state);
        self.uplink.send(state_frame(
            &tunnel_id,
            AgentTunnelState::Opened,
            &platform,
            0,
            "",
        ));
        tracing::info!(tunnel_id = %tunnel_id, %platform, "agent tunnel daemon process opened");
        let owner = self.clone();
        tokio::spawn(async move {
            owner.watch_exit(tunnel_id, platform).await;
        });
    }

    async fn watch_exit(&self, tunnel_id: String, platform: String) {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            let exited = {
                let mut state = self.state.lock().await;
                let Some(tunnel) = state.tunnels.get_mut(&tunnel_id) else {
                    return;
                };
                let overflow = tunnel.output_overflow.load(Ordering::Relaxed);
                if overflow {
                    let _ = tunnel.child.start_kill();
                }
                match tunnel.child.try_wait() {
                    Ok(Some(status)) => Some((Ok(status), overflow)),
                    Ok(None) => None,
                    Err(error) => Some((Err(error.to_string()), overflow)),
                }
            };
            if let Some((exited, overflow)) = exited {
                let pumps = {
                    let mut state = self.state.lock().await;
                    state
                        .tunnels
                        .remove(&tunnel_id)
                        .map(|tunnel| (tunnel.stdout_task, tunnel.stderr_task, tunnel.relay_task))
                };
                if let Some((stdout, stderr, relay)) = pumps {
                    let _ = stdout.await;
                    let _ = stderr.await;
                    let _ = relay.await;
                }
                if overflow {
                    self.closed(&tunnel_id, "agent tunnel output queue is full", -1);
                } else {
                    match exited {
                        Ok(status) => {
                            self.uplink.send(state_frame(
                                &tunnel_id,
                                AgentTunnelState::Closed,
                                &platform,
                                status.code().unwrap_or(-1),
                                "",
                            ));
                        }
                        Err(error) => self.closed(&tunnel_id, &error, -1),
                    }
                }
                return;
            }
        }
    }

    pub(super) fn closed(&self, tunnel_id: &str, error: &str, exit_code: i32) {
        tracing::warn!(tunnel_id, exit_code, error, "agent tunnel process closed");
        self.uplink.send(state_frame(
            tunnel_id,
            AgentTunnelState::Closed,
            "",
            exit_code,
            error,
        ));
    }
}
