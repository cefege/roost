//! Conversation actor retaining event identity beyond each ACP child lifetime.
//! ACP notifications and response markers share their dispatch-ordered channel.
//! Link loss never cancels the child; clean EOF permits immediate relaunch.

use super::{emitter::Emitter, launcher::OmpLauncher, session};
use crate::uplink::Uplink;
use agent_client_protocol::{
    Agent, Client, Error,
    v1::{ConnectionTo, schema::*},
};
use roost_proto::DOmpPrompt;
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use std::{collections::HashSet, sync::Arc, time::Duration};
use tokio::{
    sync::mpsc,
    time::{Instant, interval, sleep_until},
};

#[expect(
    clippy::large_enum_variant,
    reason = "Keep prompts inline instead of allocating a box for each message, like CoordWorkerDownstream."
)]
pub(super) enum OmpCommand {
    Prompt(DOmpPrompt),
    Cancel,
    Ack { run_id: String, seq: u64 },
    Resend,
    Close,
}
#[expect(
    clippy::large_enum_variant,
    reason = "Streaming notifications dominate this channel; boxing adds an allocation to every ACP chunk."
)]
pub(super) enum AcpEvent {
    Notification(SessionNotification),
    Completed {
        request_id: String,
        result: Result<PromptResponse, Error>,
    },
    Notice(OmpWorkerEvent),
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
    let mut tick = interval(Duration::from_millis(50));
    loop {
        let prompt = tokio::select! {
            command = inbox.recv()=>match command {
                Some(OmpCommand::Prompt(prompt)) if seen.insert(prompt.request_id.clone())=>prompt,
                Some(OmpCommand::Ack {run_id,seq})=>{emitter.ack(&run_id,seq);continue;}
                Some(OmpCommand::Resend)=>{emitter.resend();continue;}
                Some(OmpCommand::Close)|None=>{emitter.flush();return;}
                _=>continue,
            },
            _=tick.tick()=>{emitter.flush();continue;}
        };
        let (transport, description) = match launcher.launch(&prompt.cwd) {
            Ok(transport) => transport,
            Err(error) => {
                emitter.notice(
                    "error",
                    "OMP is not installed",
                    "`omp` was not found on this machine's PATH.",
                );
                emitter.emit(OmpWorkerEvent::TurnEnded {
                    request_id: prompt.request_id,
                    stop_reason: "error".into(),
                    error: Some(error.to_string()),
                    outstanding: 0,
                });
                emitter.flush();
                continue;
            }
        };
        tracing::info!(conversation_id=%emitter.conversation_id,run_id=%emitter.run_id,omp_path=description,"OMP process spawning");
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let notification_tx = events_tx.clone();
        let notice_tx = events_tx.clone();
        let result = Client.v1()
            .on_receive_notification(async move |notification:SessionNotification,_connection| {let _=notification_tx.send(AcpEvent::Notification(notification));Ok(())},agent_client_protocol::on_receive_notification!())
            .on_receive_request(async move |request:RequestPermissionRequest,responder,_connection| {
                let _=notice_tx.send(AcpEvent::Notice(OmpWorkerEvent::Notice {level:"warning".into(),title:"OMP needed approval".into(),body:format!("{}: declined because Roost runs OMP without approval prompts. Allow this tool in OMP's approval policy to use it here.",request.tool_call.fields.title.as_deref().unwrap_or("Tool"))}));
                let selected=[PermissionOptionKind::RejectOnce,PermissionOptionKind::RejectAlways].iter().find_map(|kind| request.options.iter().find(|option| option.kind==*kind).map(|option| option.option_id.clone()));
                let outcome=selected.map(|id| RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id))).unwrap_or(RequestPermissionOutcome::Cancelled);
                responder.respond(RequestPermissionResponse::new(outcome))
            },agent_client_protocol::on_receive_request!())
            .connect_with(transport,async |connection:ConnectionTo<Agent>| {connected_loop(connection,prompt,&mut emitter,&mut seen,&mut inbox,&mut events,events_tx).await}).await;
        let close = matches!(result, Ok(true));
        let error = result.err().map(|error| error.to_string());
        tracing::info!(conversation_id=%emitter.conversation_id,?error,"OMP process exited");
        emitter.emit(OmpWorkerEvent::ProcessExited { error });
        emitter.flush();
        if close {
            return;
        }
    }
}

async fn connected_loop(
    connection: ConnectionTo<Agent>,
    first: DOmpPrompt,
    emitter: &mut Emitter,
    seen: &mut HashSet<String>,
    inbox: &mut mpsc::UnboundedReceiver<OmpCommand>,
    events: &mut mpsc::UnboundedReceiver<AcpEvent>,
    sender: mpsc::UnboundedSender<AcpEvent>,
) -> Result<bool, Error> {
    let mut session = match session::open(&connection, &first, events, emitter).await {
        Ok(session) => session,
        Err(error) => {
            emitter.emit(OmpWorkerEvent::TurnEnded {
                request_id: first.request_id,
                stop_reason: "error".into(),
                error: Some(error.to_string()),
                outstanding: 0,
            });
            return Err(error);
        }
    };
    let mut outstanding = 0;
    enqueue(
        &connection,
        &mut session,
        first,
        emitter,
        &sender,
        &mut outstanding,
    )
    .await?;
    let mut tick = interval(Duration::from_millis(50));
    let mut idle = Instant::now() + Duration::from_secs(600);
    loop {
        tokio::select! {
            command=inbox.recv()=>match command {
                Some(OmpCommand::Prompt(prompt)) if seen.insert(prompt.request_id.clone())=>enqueue(&connection,&mut session,prompt,emitter,&sender,&mut outstanding).await?,
                Some(OmpCommand::Cancel)=>{tracing::info!(conversation_id=%emitter.conversation_id,"OMP cancel requested");connection.send_notification(CancelNotification::new(session.id.clone()))?;}
                Some(OmpCommand::Ack {run_id,seq})=>emitter.ack(&run_id,seq),
                Some(OmpCommand::Resend)=>emitter.resend(),
                Some(OmpCommand::Close)|None=>return Ok(true),_=>{}
            },
            Some(event)=events.recv()=>{consume_event(event,&mut session,emitter,&mut outstanding);if outstanding==0 {idle=Instant::now()+Duration::from_secs(600);}},
            _=connection.incoming_closed()=>{
                while let Ok(event)=events.try_recv() {consume_event(event,&mut session,emitter,&mut outstanding);}
                return Ok(false);
            },
            _=tick.tick()=>emitter.flush(),
            _=sleep_until(idle),if outstanding==0=>{tracing::info!(conversation_id=%emitter.conversation_id,"OMP idle shutdown");return Ok(false);}
        }
    }
}

fn consume_event(
    event: AcpEvent,
    session: &mut session::Session,
    emitter: &mut Emitter,
    outstanding: &mut u32,
) {
    match event {
        AcpEvent::Notification(notification) if notification.session_id == session.id => {
            session::update_config(session, &notification.update);
            if let Ok(update) = serde_json::to_value(notification.update) {
                emitter.emit(OmpWorkerEvent::Update { update });
            }
        }
        AcpEvent::Completed { request_id, result } => {
            *outstanding = outstanding.saturating_sub(1);
            let (stop_reason, error) = match result {
                Ok(response) => (
                    serde_json::to_value(response.stop_reason)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned))
                        .unwrap_or_else(|| "end_turn".into()),
                    None,
                ),
                Err(error) => ("error".into(), Some(error.to_string())),
            };
            tracing::info!(conversation_id=%emitter.conversation_id,%request_id,%stop_reason,"OMP turn ended");
            emitter.emit(OmpWorkerEvent::TurnEnded {
                request_id,
                stop_reason,
                error,
                outstanding: *outstanding,
            });
            emitter.flush();
        }
        AcpEvent::Notice(notice) => emitter.emit(notice),
        _ => {}
    }
}

async fn enqueue(
    connection: &ConnectionTo<Agent>,
    session: &mut session::Session,
    prompt: DOmpPrompt,
    emitter: &mut Emitter,
    sender: &mpsc::UnboundedSender<AcpEvent>,
    outstanding: &mut u32,
) -> Result<(), Error> {
    use base64::Engine;
    session::configure(connection, session, &prompt, emitter).await;
    let mut content = vec![];
    if !prompt.text.is_empty() {
        content.push(ContentBlock::Text(TextContent::new(prompt.text)));
    }
    for image in prompt.images {
        content.push(ContentBlock::Image(ImageContent::new(
            base64::engine::general_purpose::STANDARD.encode(image.data),
            image.mime_type,
        )));
    }
    for path in prompt.file_paths {
        let name = std::path::Path::new(&path)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.clone());
        content.push(ContentBlock::ResourceLink(ResourceLink::new(
            name,
            format!("file://{path}"),
        )));
    }
    let request_id = prompt.request_id;
    tracing::info!(conversation_id=%emitter.conversation_id,%request_id,"OMP turn started");
    emitter.emit(OmpWorkerEvent::TurnStarted {
        request_id: request_id.clone(),
    });
    *outstanding += 1;
    let sender = sender.clone();
    connection
        .send_request(PromptRequest::new(session.id.clone(), content))
        .on_receiving_result(async move |result| {
            let _ = sender.send(AcpEvent::Completed { request_id, result });
            Ok(())
        })
}
