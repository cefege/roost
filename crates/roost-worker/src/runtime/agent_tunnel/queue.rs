//! Arrival-order delivery of downstream tunnel frames to `AgentTunnelOwner`.
//! The dispatcher calls the port synchronously; one consumer task applies the
//! frames in the order the link delivered them, because daemon chunks must
//! hash in order and stdin bytes are a stream. Built by `runtime::owners`.

use roost_proto::{
    DAgentTunnelClose, DAgentTunnelDaemonChunk, DAgentTunnelInput, DAgentTunnelOpen,
};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;

use super::AgentTunnelOwner;

/// Frames waiting for the consumer. A daemon is ~1 MiB in 256 KiB chunks and
/// stdin is paced by the daemon's replies, so a full queue means a stuck owner.
const TUNNEL_COMMAND_CAPACITY: usize = 4096;

#[derive(Debug)]
enum TunnelCommand {
    Open(DAgentTunnelOpen),
    Input(DAgentTunnelInput),
    DaemonChunk(DAgentTunnelDaemonChunk),
    Close(DAgentTunnelClose),
    CloseAll,
}

impl TunnelCommand {
    fn tunnel_id(&self) -> Option<&str> {
        match self {
            Self::Open(frame) => Some(&frame.tunnel_id),
            Self::Input(frame) => Some(&frame.tunnel_id),
            Self::DaemonChunk(frame) => Some(&frame.tunnel_id),
            Self::Close(frame) => Some(&frame.tunnel_id),
            Self::CloseAll => None,
        }
    }
}

/// The worker's `AgentTunnelPort`: a bounded queue in front of one owner.
#[derive(Debug)]
pub struct AgentTunnelQueue {
    commands: mpsc::Sender<TunnelCommand>,
    owner: AgentTunnelOwner,
}

impl AgentTunnelQueue {
    /// Start the consumer task that applies queued frames to `owner`.
    pub fn start(owner: AgentTunnelOwner) -> Self {
        let (commands, mut queued) = mpsc::channel(TUNNEL_COMMAND_CAPACITY);
        let consumer = owner.clone();
        tokio::spawn(async move {
            while let Some(command) = queued.recv().await {
                match command {
                    TunnelCommand::Open(frame) => consumer.open(frame).await,
                    TunnelCommand::Input(frame) => consumer.input(frame).await,
                    TunnelCommand::DaemonChunk(frame) => consumer.daemon_chunk(frame).await,
                    TunnelCommand::Close(frame) => consumer.close(frame).await,
                    TunnelCommand::CloseAll => consumer.close_all().await,
                }
            }
        });
        Self { commands, owner }
    }

    fn enqueue(&self, command: TunnelCommand) {
        match self.commands.try_send(command) {
            Ok(()) => {}
            Err(TrySendError::Full(command)) => {
                let Some(tunnel_id) = command.tunnel_id().map(str::to_owned) else {
                    tracing::warn!("agent tunnel queue is full; close-all dropped");
                    return;
                };
                tracing::warn!(%tunnel_id, "agent tunnel queue is full; closing tunnel");
                // A dropped frame corrupts the stream, so the tunnel ends here.
                self.owner.closed(&tunnel_id, "worker tunnel backlog", -1);
                let owner = self.owner.clone();
                tokio::spawn(async move {
                    owner
                        .close(DAgentTunnelClose {
                            tunnel_id,
                            ..Default::default()
                        })
                        .await;
                });
            }
            Err(TrySendError::Closed(_)) => {
                tracing::warn!("agent tunnel consumer has stopped; frame dropped");
            }
        }
    }
}

impl crate::link_ports::AgentTunnelPort for AgentTunnelQueue {
    fn open(&self, request: DAgentTunnelOpen) {
        self.enqueue(TunnelCommand::Open(request));
    }
    fn input(&self, request: DAgentTunnelInput) {
        self.enqueue(TunnelCommand::Input(request));
    }
    fn daemon_chunk(&self, request: DAgentTunnelDaemonChunk) {
        self.enqueue(TunnelCommand::DaemonChunk(request));
    }
    fn close(&self, request: DAgentTunnelClose) {
        self.enqueue(TunnelCommand::Close(request));
    }
    fn close_all(&self) {
        self.enqueue(TunnelCommand::CloseAll);
    }
}
