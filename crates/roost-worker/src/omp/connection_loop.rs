//! Drives one ACP child's startup, commands, and ordered session events.
//! The conversation actor retains queued commands across failed preparation.
//! Uses the shared pending-responder lifecycle and session configuration.

use super::{
    actor::OmpCommand,
    actor_enqueue, actor_event,
    actor_types::{AcpEvent, ConnectionExit, ConnectionStart},
    emitter::Emitter,
    interaction_pending::{self, clear_interactions, respond_interaction},
    open_request::OpenRequestPump,
    session,
};
use agent_client_protocol::{
    Agent, Error, ErrorCode,
    v1::{ConnectionTo, schema::CancelNotification},
};
use roost_proto::DOmpOpen;
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use std::{collections::HashSet, time::Duration};
use tokio::{
    sync::mpsc,
    time::{Instant, interval, sleep_until},
};

pub(super) async fn connected_loop(
    connection: ConnectionTo<Agent>,
    initial: ConnectionStart<'_>,
    emitter: &mut Emitter,
    seen: &mut HashSet<String>,
    inbox: &mut mpsc::UnboundedReceiver<OmpCommand>,
    events: &mut mpsc::UnboundedReceiver<AcpEvent>,
    sender: mpsc::UnboundedSender<AcpEvent>,
) -> Result<ConnectionExit, Error> {
    let ConnectionStart {
        prompt: first,
        initial_ui,
        ui,
        progress,
        pending: pending_interactions,
        deferred,
    } = initial;
    let mut outstanding = 0;
    let mut session = match session::open(
        &connection,
        &first,
        OpenRequestPump {
            events,
            inbox,
            pending: pending_interactions,
            deferred,
            emitter,
        },
        progress.catalog.as_ref(),
    )
    .await
    {
        Ok(session) => session,
        Err(error) => {
            clear_interactions(pending_interactions, emitter);
            let stopped = error.code == ErrorCode::from(-32800);
            if progress.prompt_first {
                emitter.emit(OmpWorkerEvent::TurnEnded {
                    request_id: first.request_id,
                    stop_reason: if stopped { "cancelled" } else { "error" }.into(),
                    error: (!stopped).then(|| error.to_string()),
                    outstanding: 0,
                });
            }
            if stopped {
                tracing::info!(conversation_id=%emitter.conversation_id, "OMP startup stopped");
                for command in deferred.drain(..) {
                    match command {
                        OmpCommand::Prompt(prompt) => emitter.emit(OmpWorkerEvent::TurnEnded {
                            request_id: prompt.request_id,
                            stop_reason: "cancelled".into(),
                            error: None,
                            outstanding: 0,
                        }),
                        OmpCommand::Ack { run_id, seq } => emitter.ack(&run_id, seq),
                        OmpCommand::Resend => emitter.resend(),
                        _ => {}
                    }
                }
                return Ok(ConnectionExit::Idle);
            }
            return Err(error);
        }
    };
    if !progress.prompt_first && !first.mode.is_empty() {
        let _ = session::set_mode(&connection, &mut session, first.mode.clone(), emitter).await;
    }
    if progress.prompt_first {
        actor_enqueue::enqueue(
            &connection,
            &mut session,
            first,
            emitter,
            &sender,
            &mut outstanding,
        )
        .await?;
    }
    ui.reconnect(&connection, &session, emitter, &sender, initial_ui);
    let mut lease_tick = interval(Duration::from_secs(1));
    let mut pending_refresh = None;
    let mut tick = interval(Duration::from_millis(50));
    let mut idle = Instant::now() + Duration::from_secs(600);
    loop {
        tokio::select! {
            command=async {
                if let Some(command) = deferred.pop_front() {
                    Some(command)
                } else {
                    inbox.recv().await
                }
            }=>match command {
                Some(OmpCommand::Prompt(prompt)) => {
                    if seen.insert(prompt.request_id.clone()) {
                        progress.has_started_turn = true;
                        actor_enqueue::enqueue(&connection,&mut session,prompt,emitter,&sender,&mut outstanding).await?;
                    }
                }
                Some(OmpCommand::Open(request)) => {
                    let prompt = DOmpOpen {
                        conversation_id: request.conversation_id,
                        cwd: session.cwd.clone(),
                        acp_session_id: session.resume_id.clone(),
                        ..Default::default()
                    };
                    if outstanding == 0 && !ui.streaming() {
                        return Ok(ConnectionExit::Refresh(prompt));
                    } else {
                        pending_refresh = Some(prompt);
                    }
                }
                Some(OmpCommand::SetMode(request)) => {
                    if request.conversation_id == emitter.conversation_id {
                        if outstanding == 0 && !ui.streaming() {
                            let _ = session::set_mode(
                                &connection,
                                &mut session,
                                request.mode,
                                emitter,
                            )
                            .await;
                        } else {
                            emitter.notice(
                                "warning",
                                "Mode not applied",
                                "OMP session mode cannot change while a prompt is running.",
                            );
                        }
                    }
                }
                Some(OmpCommand::Ui(request)) => {
                    ui.request(request, &connection, &session, emitter, &sender);
                }
                Some(OmpCommand::Cancel)=>{
                    tracing::info!(conversation_id=%emitter.conversation_id,"OMP cancel requested");
                    clear_interactions(pending_interactions, emitter);
                    connection.send_notification(CancelNotification::new(session.id.clone()))?;
                }
                Some(OmpCommand::Respond(response)) => {
                    respond_interaction(response, pending_interactions, emitter);
                }
                Some(OmpCommand::Ack {run_id,seq})=>{
                    if run_id == emitter.run_id {
                        emitter.ack(&run_id,seq);
                        ui.acknowledge(seq,&connection,&session,&sender,emitter);
                    }
                },
                Some(OmpCommand::Resend)=>emitter.resend(),
                Some(OmpCommand::Close)|None=>{
                    clear_interactions(pending_interactions, emitter);
                    return Ok(ConnectionExit::Closed);
                },
            },
            Some(event)=events.recv()=>{
                match event {
                    AcpEvent::Ui(payload) => ui.notification(payload,&session,emitter),
                    AcpEvent::UiState(payload) => {
                        if payload.get("resetHistory").and_then(serde_json::Value::as_bool) == Some(true) {
                            clear_interactions(pending_interactions,emitter);
                        }
                        if ui.session_state(payload,&mut session,emitter,outstanding) {
                            pending_refresh = Some(DOmpOpen {
                                conversation_id: emitter.conversation_id.clone(),
                                cwd: session.cwd.clone(),
                                acp_session_id: session.resume_id.clone(),
                                ..Default::default()
                            });
                        }
                        progress.has_started_turn |= ui.streaming();
                    }
                    AcpEvent::HostService(payload) => ui.host_service(payload,&session,&connection,emitter),
                    AcpEvent::HostServiceCancel(payload) => ui.host_service_cancel(payload,&session,emitter),
                    AcpEvent::UiCompleted {method,transport_id,request_id,result} => ui.complete(method,&transport_id,&request_id,result,emitter),
                    event @ AcpEvent::Interaction { .. } => {
                        interaction_pending::consume_open_event(
                            event,
                            pending_interactions,
                            emitter,
                        );
                    }
                    event => {
                        if matches!(&event, AcpEvent::Completed { .. }) && outstanding <= 1 {
                            clear_interactions(pending_interactions, emitter);
                        }
                        actor_event::consume_event(event,&mut session,emitter,&mut outstanding);
                    }
                }
                if outstanding==0 && !ui.streaming() {
                    idle=Instant::now()+Duration::from_secs(600);
                    if let Some(prompt) = pending_refresh.take() {
                        clear_interactions(pending_interactions, emitter);
                        return Ok(ConnectionExit::Refresh(prompt));
                    }
                }
            },
            _=connection.incoming_closed()=>{
                actor_event::drain_disconnect(
                    events,
                    &mut session,
                    emitter,
                    &mut outstanding,
                    pending_interactions,
                );
                clear_interactions(pending_interactions, emitter);
                return Ok(ConnectionExit::Idle);
            },
            _=tick.tick()=>{
                emitter.flush();
            },
            _=lease_tick.tick(),if ui.active()=>{
                ui.expire(&connection,&session,&sender,emitter);
            },
            _=sleep_until(idle),if outstanding==0 && !ui.active() && !ui.streaming()=>{
                tracing::info!(conversation_id=%emitter.conversation_id,"OMP idle shutdown");
                clear_interactions(pending_interactions, emitter);
                return Ok(ConnectionExit::Idle);
            }
        }
    }
}
