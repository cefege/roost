//! Conversation actor retaining event identity beyond each ACP child lifetime.
//! ACP notifications and response markers share their dispatch-ordered channel.
//! Link loss never cancels the child; clean EOF permits immediate relaunch.

use super::interaction_pending::{PendingInteractions, clear_interactions, drain_pending_events};
use super::{
    actor_types::{AcpEvent, ConnectionExit, ConnectionProgress, ConnectionStart},
    connection_loop,
    emitter::Emitter,
    launcher::{OmpLaunch, OmpLauncher},
};
use crate::uplink::Uplink;
use agent_client_protocol::{
    Agent, Client,
    v1::{ConnectionTo, Responder, schema::*},
};
use roost_proto::{DOmpOpen, DOmpPrompt};
use roost_protocol::wire::agent_chat::{OmpInteraction, OmpWorkerEvent};
use std::{
    collections::{HashSet, VecDeque},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::mpsc, time::interval};

pub(super) enum OmpCommand {
    Open(DOmpOpen),
    Prompt(DOmpPrompt),
    Respond(roost_proto::DOmpRespond),
    SetMode(roost_proto::DOmpSetMode),
    Cancel,
    Ack { run_id: String, seq: u64 },
    Resend,
    Close,
}

pub(super) async fn conversation_actor<L: OmpLauncher>(
    launcher: Arc<L>,
    uplink: Uplink,
    id: String,
    run_id: String,
    mut inbox: mpsc::UnboundedReceiver<OmpCommand>,
) {
    let mut emitter = Emitter::new(id, run_id, uplink);
    let mut seen = HashSet::new();
    let mut deferred = VecDeque::new();
    let mut child_generation = 0_u64;
    let mut tick = interval(Duration::from_millis(50));
    let mut refresh: Option<DOmpOpen> = None;
    loop {
        let (prompt, start_turn) = if let Some(request) = refresh.take() {
            (
                DOmpPrompt {
                    conversation_id: request.conversation_id,
                    cwd: request.cwd,
                    acp_session_id: request.acp_session_id,
                    ..Default::default()
                },
                false,
            )
        } else {
            tokio::select! {
                command = async {
                    if let Some(command) = deferred.pop_front() {
                        Some(command)
                    } else {
                        inbox.recv().await
                    }
                }=>match command {
                    Some(OmpCommand::Open(request)) => (DOmpPrompt {
                        conversation_id: request.conversation_id,
                        cwd: request.cwd,
                        acp_session_id: request.acp_session_id,
                        ..Default::default()
                    }, false),
                    Some(OmpCommand::SetMode(request)) => (DOmpPrompt {
                        conversation_id: request.conversation_id,
                        cwd: request.cwd,
                        acp_session_id: request.acp_session_id,
                        mode: request.mode,
                        ..Default::default()
                    }, false),
                    Some(OmpCommand::Prompt(prompt)) if seen.insert(prompt.request_id.clone())=>(prompt, true),
                    Some(OmpCommand::Ack {run_id,seq})=>{emitter.ack(&run_id,seq);continue;}
                    Some(OmpCommand::Resend)=>{emitter.resend();continue;}
                    Some(OmpCommand::Close)|None=>{emitter.flush();return;}
                    _=>continue,
                },
                _=tick.tick()=>{emitter.flush();continue;}
            }
        };
        let OmpLaunch {
            transport,
            description,
            catalog,
        } = match launcher.launch(&prompt.cwd) {
            Ok(transport) => transport,
            Err(error) => {
                if start_turn {
                    emitter.notice("error", "OMP could not start", &error.to_string());

                    emitter.emit(OmpWorkerEvent::TurnEnded {
                        request_id: prompt.request_id,
                        stop_reason: "error".into(),
                        error: Some(error.to_string()),
                        outstanding: 0,
                    });
                } else {
                    emitter.emit(OmpWorkerEvent::SessionFailed {
                        error: error.to_string(),
                    });
                }
                emitter.flush();
                continue;
            }
        };
        tracing::info!(conversation_id=%emitter.conversation_id,run_id=%emitter.run_id,omp_path=description,"OMP process spawning");
        child_generation = child_generation.saturating_add(1);
        let identity_prefix = format!("{}:{child_generation}", emitter.run_id);
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let notification_tx = events_tx.clone();
        let permission_tx = events_tx.clone();
        let elicitation_tx = events_tx.clone();
        let mut progress = ConnectionProgress {
            prompt_first: start_turn,
            has_started_turn: start_turn,
            catalog,
        };
        let permission_identity_prefix = identity_prefix.clone();
        let elicitation_identity_prefix = identity_prefix;
        let result = Client.v1()
            .on_receive_notification(async move |notification:SessionNotification,_connection| {let _=notification_tx.send(AcpEvent::Notification(notification));Ok(())},agent_client_protocol::on_receive_notification!())
            .on_receive_request(async move |request: RequestPermissionRequest, responder: Responder<RequestPermissionResponse>, _connection| {
                let request_id = format!("{permission_identity_prefix}:{}", responder.id());
                let mut payload = serde_json::to_value(&request).unwrap_or_default();
                if let Some(fields) = payload.as_object_mut() {
                    fields.insert("kind".into(), serde_json::Value::String("permission".into()));
                }
                let interaction = OmpInteraction { id: request_id.clone(), request: payload };
                let allowed = request.options.iter().map(|option| option.option_id.to_string()).collect::<HashSet<_>>();
                let _ = permission_tx.send(AcpEvent::Interaction {
                    interaction,
                    respond: Box::new(move |decision| {
                        let selected = if decision.request_id == request_id
                            && decision.action == "permission"
                            && let Ok(content) = serde_json::from_str::<serde_json::Value>(&decision.content_json)
                        {
                            content.get("optionId").and_then(serde_json::Value::as_str)
                                .filter(|option| allowed.contains(*option))
                                .map(str::to_owned)
                        } else { None };
                        let outcome = selected
                            .map(|id| RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)))
                            .unwrap_or(RequestPermissionOutcome::Cancelled);
                        responder.respond(RequestPermissionResponse::new(outcome))
                    }),
                });
                Ok(())
            }, agent_client_protocol::on_receive_request!())
            .on_receive_request(async move |request: CreateElicitationRequest, responder: Responder<CreateElicitationResponse>, _connection| {
                let request_id = format!("{elicitation_identity_prefix}:{}", responder.id());
                let mut payload = serde_json::to_value(&request).unwrap_or_default();
                if let Some(fields) = payload.as_object_mut() {
                    fields.insert("kind".into(), serde_json::Value::String("form".into()));
                }
                let interaction = OmpInteraction { id: request_id.clone(), request: payload };
                let _ = elicitation_tx.send(AcpEvent::Interaction {
                    interaction,
                    respond: Box::new(move |decision| {
                        let action = match decision.action.as_str() {
                            "accept" => serde_json::json!({
                                "action": "accept",
                                "content": serde_json::from_str::<serde_json::Value>(&decision.content_json).unwrap_or_default(),
                            }),
                            "decline" | "cancel" => serde_json::json!({"action":decision.action}),
                            _ => serde_json::json!({"action":"cancel"}),
                        };
                        let response = serde_json::from_value::<CreateElicitationResponse>(action)
                            .unwrap_or_else(|_| CreateElicitationResponse::new(ElicitationAction::Cancel));
                        responder.respond(response)
                    }),
                });
                Ok(())
            }, agent_client_protocol::on_receive_request!())
            .connect_with(transport, async |connection: ConnectionTo<Agent>| {
                let mut pending = PendingInteractions::new();
                let result = connection_loop::connected_loop(
                    connection,
                    ConnectionStart { prompt, progress: &mut progress, pending: &mut pending, deferred: &mut deferred },
                    &mut emitter,
                    &mut seen,
                    &mut inbox,
                    &mut events,
                    events_tx,
                )
                .await;
                drain_pending_events(&mut events, &mut pending, &mut emitter);
                clear_interactions(&mut pending, &mut emitter);
                result
            })
            .await;
        if let Ok(ConnectionExit::Refresh(prompt)) = result {
            tracing::info!(conversation_id=%emitter.conversation_id,"OMP process reloading its provider configuration");
            emitter.flush();
            refresh = Some(prompt);
            continue;
        }
        let close = matches!(result, Ok(ConnectionExit::Closed));
        let error = result.err().map(|error| error.to_string());
        tracing::info!(conversation_id=%emitter.conversation_id,?error,"OMP process exited");
        if progress.has_started_turn {
            emitter.emit(OmpWorkerEvent::ProcessExited { error });
        } else if let Some(error) = error {
            emitter.emit(OmpWorkerEvent::SessionFailed { error });
        }
        emitter.flush();
        if close {
            return;
        }
    }
}
