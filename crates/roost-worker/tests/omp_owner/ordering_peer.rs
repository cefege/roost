//! Controlled ACP peers for process lifetime and callback-order regressions.
//! The owner uses SDK byte streams; request responders stay owned by SDK tasks.
//! Tests release wire exchanges explicitly instead of depending on timer races.

use agent_client_protocol::{
    Agent, ByteStreams, Client,
    v1::{ConnectionTo, Responder, schema::*},
};
use roost_worker::omp::launcher::{OmpLaunch, OmpLaunchError, OmpLauncher};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::{
    io::{DuplexStream, ReadHalf, WriteHalf},
    sync::Notify,
};
use tokio_util::compat::{Compat, TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

type Transport = ByteStreams<Compat<WriteHalf<DuplexStream>>, Compat<ReadHalf<DuplexStream>>>;
type HeldPrompts = Arc<Mutex<Vec<(SessionId, Responder<PromptResponse>)>>>;

#[derive(Debug, Clone, Copy)]
pub enum Scenario {
    Complete,
    StallInitialize,
    StallSession,
    OrderedPrompts,
}

#[derive(Debug)]
pub struct OrderingLauncher {
    pub scenario: Scenario,
    pub started: Arc<Notify>,
    pub queued: Arc<Notify>,
    pub release: Arc<Notify>,
    pub disconnect: Arc<Notify>,
    pub closed: Arc<Notify>,
    pub launches: AtomicUsize,
}

impl OrderingLauncher {
    pub fn new(scenario: Scenario) -> Self {
        Self {
            scenario,
            started: Arc::new(Notify::new()),
            queued: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            disconnect: Arc::new(Notify::new()),
            closed: Arc::new(Notify::new()),
            launches: AtomicUsize::new(0),
        }
    }
}

impl OmpLauncher for OrderingLauncher {
    type Transport = Transport;

    fn launch(&self, _cwd: &str) -> Result<OmpLaunch<Transport>, OmpLaunchError> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let (client, agent) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client);
        let (agent_read, agent_write) = tokio::io::split(agent);
        let scenario = self.scenario;
        let initialize_started = Arc::clone(&self.started);
        let session_started = Arc::clone(&self.started);
        let queued = Arc::clone(&self.queued);
        let release = Arc::clone(&self.release);
        let disconnect = Arc::clone(&self.disconnect);
        let closed = Arc::clone(&self.closed);
        let held: HeldPrompts = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(async move {
            let connection = Agent
                .v1()
                .on_receive_request(
                    async move |request: InitializeRequest, responder, cx: ConnectionTo<Client>| {
                        if matches!(scenario, Scenario::StallInitialize) {
                            initialize_started.notify_one();
                            return cx.spawn(async move {
                                std::future::pending::<()>().await;
                                responder.respond(InitializeResponse::new(request.protocol_version))
                            });
                        }
                        responder.respond(InitializeResponse::new(request.protocol_version))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |_request: NewSessionRequest,
                                responder,
                                cx: ConnectionTo<Client>| {
                        if matches!(scenario, Scenario::StallSession) {
                            session_started.notify_one();
                            return cx.spawn(async move {
                                std::future::pending::<()>().await;
                                responder.respond(NewSessionResponse::new("s1"))
                            });
                        }
                        responder.respond(NewSessionResponse::new("s1"))
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                        if !matches!(scenario, Scenario::OrderedPrompts) {
                            return responder.respond(PromptResponse::new(StopReason::EndTurn));
                        }
                        let prompts = {
                            let mut held = held.lock().unwrap();
                            held.push((request.session_id, responder));
                            if held.len() != 2 {
                                return Ok(());
                            }
                            std::mem::take(&mut *held)
                        };
                        queued.notify_one();
                        let release = Arc::clone(&release);
                        let sender = cx.clone();
                        cx.spawn(async move {
                            release.notified().await;
                            for ((session_id, responder), chunk) in
                                prompts.into_iter().zip(["A", "B"])
                            {
                                sender.send_notification(SessionNotification::new(
                                    session_id,
                                    SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                        ContentBlock::Text(TextContent::new(chunk)),
                                    )),
                                ))?;
                                responder.respond(PromptResponse::new(StopReason::EndTurn))?;
                            }
                            Ok(())
                        })
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .connect_to(ByteStreams::new(
                    agent_write.compat_write(),
                    agent_read.compat(),
                ));
            tokio::select! {
                _ = connection => {}
                _ = disconnect.notified() => {}
            }
            closed.notify_one();
        });
        Ok(OmpLaunch {
            transport: ByteStreams::new(client_write.compat_write(), client_read.compat()),
            description: "ordering duplex OMP".into(),
            catalog: None,
        })
    }
}
