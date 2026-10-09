//! The worker-link arms relaying opaque agent environment tunnels.
//!
//! Called after the link dispatcher has authenticated the current worker; the
//! tunnel registry applies its own generation and tunnel-id fence before relay.

use roost_protocol::wire::coord_worker::{AgentTunnelState, CoordWorkerUpstream};

use crate::worker_link::dispatch::DispatchOutcome;
use crate::worker_link::frame_dispatch::WorkerFrameDispatcher;

impl WorkerFrameDispatcher {
    pub(super) fn handle_agent_tunnel_live(
        &self,
        worker_fp: &str,
        frame: CoordWorkerUpstream,
    ) -> DispatchOutcome {
        match frame {
            CoordWorkerUpstream::AgentTunnelState(state) => {
                let registry = &self.core.services.agent_host.tunnels;
                match state.state {
                    AgentTunnelState::Opened => {
                        registry.send_from_worker(
                            worker_fp,
                            &state.tunnel_id,
                            axum::extract::ws::Message::Text(
                                serde_json::json!({"type":"opened"}).to_string().into(),
                            ),
                        );
                    }
                    AgentTunnelState::NeedDaemon => {
                        registry.send_from_worker(
                            worker_fp,
                            &state.tunnel_id,
                            axum::extract::ws::Message::Text(
                                serde_json::json!({"type":"need_daemon", "platform":state.platform})
                                    .to_string().into(),
                            ),
                        );
                    }
                    AgentTunnelState::Closed => {
                        let close = axum::extract::ws::Message::Close(Some(
                            axum::extract::ws::CloseFrame {
                                code: if state.error.is_empty() {
                                    1000_u16
                                } else {
                                    1011_u16
                                },
                                reason: if state.error.is_empty() {
                                    format!("exit {}", state.exit_code).into()
                                } else {
                                    state.error.clone().into()
                                },
                            },
                        ));
                        registry.send_from_worker(worker_fp, &state.tunnel_id, close);
                        registry.remove(&state.tunnel_id);
                    }
                }
            }
            CoordWorkerUpstream::AgentTunnelOutput(output) => {
                let message = if output.stderr {
                    let text = String::from_utf8_lossy(&output.data).into_owned();
                    axum::extract::ws::Message::Text(
                        serde_json::json!({"type":"stderr","text":text})
                            .to_string()
                            .into(),
                    )
                } else {
                    axum::extract::ws::Message::Binary(output.data.into())
                };
                self.core.services.agent_host.tunnels.send_from_worker(
                    worker_fp,
                    &output.tunnel_id,
                    message,
                );
            }
            _ => return DispatchOutcome::Refused,
        }
        DispatchOutcome::Handled
    }
}
