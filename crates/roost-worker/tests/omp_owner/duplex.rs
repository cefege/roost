//! Real SDK agent over duplex byte streams for OMP owner scenarios.
//! Fixed exchanges exercise protocol dispatch rather than mocking actor calls.
//! A held prompt completes only after its session receives cancellation.

use agent_client_protocol::{
    Agent, ByteStreams, Client,
    v1::{ConnectionTo, schema::*},
};
use roost_worker::omp::launcher::{OmpLaunchError, OmpLauncher};
use std::sync::Arc;
use tokio::{
    io::{DuplexStream, ReadHalf, WriteHalf},
    sync::Notify,
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Transport = ByteStreams<Compat<WriteHalf<DuplexStream>>, Compat<ReadHalf<DuplexStream>>>;

#[derive(Debug, Default)]
pub struct DuplexLauncher {
    pub hold: bool,
    pub started: Arc<Notify>,
    pub cancelled: Arc<Notify>,
}
impl OmpLauncher for DuplexLauncher {
    type Transport = Transport;
    fn launch(&self, _cwd: &str) -> Result<(Transport, String), OmpLaunchError> {
        let (client, agent) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client);
        let (agent_read, agent_write) = tokio::io::split(agent);
        let hold = self.hold;
        let started = Arc::clone(&self.started);
        let cancelled = Arc::clone(&self.cancelled);
        let release = Arc::new(Notify::new());
        let release_prompt = Arc::clone(&release);
        tokio::spawn(async move {
            let _ = Agent
                .v1()
                .on_receive_request(
                    async |request: InitializeRequest, responder, _cx| {
                        responder.respond(
                            InitializeResponse::new(request.protocol_version)
                                .agent_capabilities(AgentCapabilities::new().load_session(true)),
                        )
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async |_request: NewSessionRequest, responder, _cx| {
                        responder.respond(NewSessionResponse::new("s1"))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async |request: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                        cx.send_notification(SessionNotification::new(
                            request.session_id,
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("old")),
                            )),
                        ))?;
                        responder.respond(LoadSessionResponse::new())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                        started.notify_one();
                        cx.send_notification(SessionNotification::new(
                            request.session_id,
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("hi")),
                            )),
                        ))?;
                        let release = Arc::clone(&release_prompt);
                        cx.spawn(async move {
                            if hold {
                                release.notified().await;
                            }
                            responder.respond(PromptResponse::new(if hold {
                                StopReason::Cancelled
                            } else {
                                StopReason::EndTurn
                            }))
                        })
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_notification(
                    async move |_request: CancelNotification, _cx| {
                        cancelled.notify_one();
                        release.notify_one();
                        Ok(())
                    },
                    agent_client_protocol::on_receive_notification!(),
                )
                .connect_to(ByteStreams::new(
                    agent_write.compat_write(),
                    agent_read.compat(),
                ))
                .await;
        });
        Ok((
            ByteStreams::new(client_write.compat_write(), client_read.compat()),
            "duplex OMP".into(),
        ))
    }
}

#[derive(Debug)]
pub struct MissingLauncher;
impl OmpLauncher for MissingLauncher {
    type Transport = Transport;
    fn launch(&self, _cwd: &str) -> Result<(Transport, String), OmpLaunchError> {
        Err(OmpLaunchError::NotInstalled)
    }
}
