//! Converts queued browser content into ordered ACP prompt requests.
//! Session configuration is applied before the prompt enters the stream.

use super::{actor_types::AcpEvent, emitter::Emitter, session};
use agent_client_protocol::{
    Agent, Error,
    v1::{ConnectionTo, schema::*},
};
use roost_proto::DOmpPrompt;
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use tokio::sync::mpsc;

pub(super) async fn enqueue(
    connection: &ConnectionTo<Agent>,
    session: &mut session::Session,
    prompt: DOmpPrompt,
    emitter: &mut Emitter,
    sender: &mpsc::UnboundedSender<AcpEvent>,
    outstanding: &mut u32,
) -> Result<(), Error> {
    use base64::Engine;
    if !prompt.mode.is_empty() && *outstanding > 0 {
        emitter.notice(
            "warning",
            "Plan prompt not submitted",
            "OMP session mode cannot change while another prompt is running.",
        );
        emitter.emit(OmpWorkerEvent::TurnEnded {
            request_id: prompt.request_id,
            stop_reason: "error".into(),
            error: Some("OMP session mode cannot change while another prompt is running.".into()),
            outstanding: *outstanding,
        });
        return Ok(());
    }
    if !session::configure(connection, session, &prompt, emitter).await {
        let error = "OMP did not apply the requested session mode".to_owned();
        emitter.emit(OmpWorkerEvent::TurnEnded {
            request_id: prompt.request_id,
            stop_reason: "error".into(),
            error: Some(error),
            outstanding: *outstanding,
        });
        return Ok(());
    }
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
