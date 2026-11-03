//! Real SDK agent over duplex byte streams for OMP owner scenarios.
//! Fixed exchanges exercise protocol dispatch rather than mocking actor calls.
//! A held prompt completes only after its session receives cancellation.

use agent_client_protocol::{
    Agent, ByteStreams, Client,
    v1::{ConnectionTo, schema::*},
};
use roost_worker::omp::launcher::{OmpLaunch, OmpLaunchError, OmpLauncher};
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
    pub loads: Arc<std::sync::atomic::AtomicUsize>,
    pub mode_changes: Arc<std::sync::atomic::AtomicUsize>,
    pub require_mode_before_prompt: bool,
    pub elicitation: bool,
    pub elicitation_outcome: Arc<std::sync::atomic::AtomicUsize>,
    pub elicitation_on_new_session: bool,
    pub delay_new_session_form: bool,
    pub new_session_started: Arc<Notify>,
    pub release_new_session_form: Arc<Notify>,
}
impl OmpLauncher for DuplexLauncher {
    type Transport = Transport;
    fn launch(&self, _cwd: &str) -> Result<OmpLaunch<Transport>, OmpLaunchError> {
        let (client, agent) = tokio::io::duplex(64 * 1024);
        let (client_read, client_write) = tokio::io::split(client);
        let (agent_read, agent_write) = tokio::io::split(agent);
        let hold = self.hold;
        let started = Arc::clone(&self.started);
        let cancelled = Arc::clone(&self.cancelled);
        let loads = Arc::clone(&self.loads);
        let mode_changes = Arc::clone(&self.mode_changes);
        let require_mode_before_prompt = self.require_mode_before_prompt;
        let elicitation = self.elicitation;
        let elicitation_on_new_session = self.elicitation_on_new_session;
        let delay_new_session_form = self.delay_new_session_form;
        let new_session_started = Arc::clone(&self.new_session_started);
        let release_new_session_form = Arc::clone(&self.release_new_session_form);
        let elicitation_outcome = Arc::clone(&self.elicitation_outcome);
        let new_session_outcome = Arc::clone(&elicitation_outcome);
        let release = Arc::new(Notify::new());
        let prompt_mode_changes = Arc::clone(&mode_changes);
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
                    async move |_request: NewSessionRequest, responder, cx: ConnectionTo<Client>| {
                        let new_session_started = Arc::clone(&new_session_started);
                        let release_new_session_form = Arc::clone(&release_new_session_form);
                        let new_session_outcome = Arc::clone(&new_session_outcome);
                        let task_connection = cx.clone();
                        cx.spawn(async move {
                        new_session_started.notify_one();
                        if delay_new_session_form {
                            release_new_session_form.notified().await;
                        }
                        if elicitation_on_new_session {
                            let schema = serde_json::from_value(serde_json::json!({
                                "type":"object",
                                "properties":{"ok":{"type":"boolean"}}
                            })).expect("startup elicitation schema");
                            let response = task_connection.send_request(CreateElicitationRequest::new(
                                ElicitationFormMode::new(
                                    ElicitationSessionScope::new("s1"),
                                    schema,
                                ),
                                "Please confirm before session creation",
                            )).block_task().await?;
                            new_session_outcome.store(
                                if matches!(response.action, ElicitationAction::Accept(_)) { 1 } else { 3 },
                                std::sync::atomic::Ordering::Relaxed,
                            );
                        }
                        let mut response = NewSessionResponse::new("s1");
                        response.config_options = Some(serde_json::from_value(serde_json::json!([
                            {"id":"model","name":"Model","type":"select","currentValue":"provider/model-a",
                             "options":[{"value":"provider/model-a","name":"Model A"},{"value":"provider/model-b","name":"Model B"}]},
                            {"id":"thinking","name":"Thinking","type":"select","currentValue":"medium",
                             "options":[{"value":"low","name":"Low"},{"value":"medium","name":"Medium"}]}
                        ])).expect("session options"));
                        response.modes = Some(SessionModeState::new(
                            "build",
                            vec![SessionMode::new("build", "Build"), SessionMode::new("plan", "Plan")],
                        ));
                        responder.respond(response)
                        })
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: LoadSessionRequest, responder, cx: ConnectionTo<Client>| {
                        assert_eq!(request.session_id.to_string(), "s1");
                        loads.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        cx.send_notification(SessionNotification::new(
                            request.session_id,
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("old")),
                            )),
                        ))?;
                        let mut response = LoadSessionResponse::new();
                        response.config_options = Some(serde_json::from_value(serde_json::json!([
                            {"id":"model","name":"Model","type":"select","currentValue":"provider/model-a",
                             "options":[{"value":"provider/model-a","name":"Model A"},{"value":"provider/model-b","name":"Model B"}]},
                            {"id":"thinking","name":"Thinking","type":"select","currentValue":"medium",
                             "options":[{"value":"low","name":"Low"},{"value":"medium","name":"Medium"}]}
                        ])).expect("loaded session options"));
                        responder.respond(response)
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: SetSessionModeRequest, responder, cx: ConnectionTo<Client>| {
                        mode_changes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        cx.send_notification(SessionNotification::new(
                            request.session_id,
                            SessionUpdate::CurrentModeUpdate(CurrentModeUpdate::new(request.mode_id)),
                        ))?;
                        responder.respond(SetSessionModeResponse::new())
                    },
                    agent_client_protocol::on_receive_request!(),
                )
                .on_receive_request(
                    async move |request: PromptRequest, responder, cx: ConnectionTo<Client>| {
                        let started = Arc::clone(&started);
                        let prompt_mode_changes = Arc::clone(&prompt_mode_changes);
                        let elicitation_outcome = Arc::clone(&elicitation_outcome);
                        let release = Arc::clone(&release_prompt);
                        let task_connection = cx.clone();
                        cx.spawn(async move {
                        started.notify_one();
                        if require_mode_before_prompt {
                            assert!(
                                prompt_mode_changes.load(std::sync::atomic::Ordering::Relaxed) > 0,
                                "ACP mode request must finish before the prompt request",
                            );
                        }
                        if elicitation {
                            let schema = serde_json::from_value(serde_json::json!({
                                "type":"object",
                                "properties":{"ok":{"type":"boolean"}}
                            }))
                            .expect("elicitation schema");
                            let request = CreateElicitationRequest::new(
                                ElicitationFormMode::new(
                                    ElicitationSessionScope::new(request.session_id.clone()),
                                    schema,
                                ),
                                "Please confirm",
                            );
                            let response = task_connection.send_request(request).block_task().await?;
                            let outcome = if matches!(response.action, ElicitationAction::Accept(_)) {
                                1
                            } else {
                                3
                            };
                            elicitation_outcome.store(outcome, std::sync::atomic::Ordering::Relaxed);
                        }
                        task_connection.send_notification(SessionNotification::new(
                            request.session_id,
                            SessionUpdate::AgentMessageChunk(ContentChunk::new(
                                ContentBlock::Text(TextContent::new("hi")),
                            )),
                        ))?;
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
        Ok(OmpLaunch {
            transport: ByteStreams::new(client_write.compat_write(), client_read.compat()),
            description: "duplex OMP".into(),
            catalog: None,
        })
    }
}

#[derive(Debug)]
pub struct MissingLauncher;
impl OmpLauncher for MissingLauncher {
    type Transport = Transport;
    fn launch(&self, _cwd: &str) -> Result<OmpLaunch<Transport>, OmpLaunchError> {
        Err(OmpLaunchError::NotInstalled)
    }
}
