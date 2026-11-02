//! ACP initialization, history-suppressed session loading and prompt settings.
//! Called inside a supervised connection; OMP keeps its own local tools/login.
//! A failed load starts a new session with an explicit context-loss notice.

use super::emitter::Emitter;
use agent_client_protocol::{
    Agent, Error, ErrorCode,
    v1::{ConnectionTo, schema::*},
};
use roost_proto::DOmpPrompt;
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use tokio::sync::mpsc;

pub(super) struct Session {
    pub id: SessionId,
    pub model: Option<String>,
    pub thinking: Option<String>,
}

pub(super) async fn open(
    connection: &ConnectionTo<Agent>,
    prompt: &DOmpPrompt,
    notifications: &mut mpsc::UnboundedReceiver<super::actor::AcpEvent>,
    emitter: &mut Emitter,
) -> Result<Session, Error> {
    connection
        .send_request(InitializeRequest::new(
            agent_client_protocol::schema::ProtocolVersion::V1,
        ))
        .block_task()
        .await?;
    let result = open_authenticated(connection, prompt, notifications, emitter).await;
    if result
        .as_ref()
        .is_err_and(|error| error.code == ErrorCode::AUTH_REQUIRED)
    {
        emitter.notice(
            "error",
            "OMP is not signed in",
            "Run `omp login` on this machine.",
        );
    }
    result
}

async fn open_authenticated(
    connection: &ConnectionTo<Agent>,
    prompt: &DOmpPrompt,
    notifications: &mut mpsc::UnboundedReceiver<super::actor::AcpEvent>,
    emitter: &mut Emitter,
) -> Result<Session, Error> {
    let mut authenticated = false;
    if !prompt.acp_session_id.is_empty() {
        loop {
            let loaded = connection
                .send_request(LoadSessionRequest::new(
                    prompt.acp_session_id.clone(),
                    prompt.cwd.clone(),
                ))
                .block_task()
                .await;
            match loaded {
                Ok(response) => {
                    discard_replay(notifications, emitter);
                    tracing::info!(conversation_id=%prompt.conversation_id,acp_session_id=%prompt.acp_session_id,"OMP session loaded");
                    return Ok(ready(
                        prompt.acp_session_id.clone().into(),
                        response.config_options,
                        emitter,
                    ));
                }
                Err(error) if error.code == ErrorCode::AUTH_REQUIRED && !authenticated => {
                    authenticated = true;
                    connection
                        .send_request(AuthenticateRequest::new("agent"))
                        .block_task()
                        .await?;
                }
                Err(error) if error.code == ErrorCode::AUTH_REQUIRED => return Err(error),
                Err(_) => {
                    discard_replay(notifications, emitter);
                    emitter.notice("warning","Started a new OMP session","OMP could not reopen the previous session, so earlier messages are not in its context.");
                    break;
                }
            }
        }
    }
    loop {
        match connection
            .send_request(NewSessionRequest::new(prompt.cwd.clone()))
            .block_task()
            .await
        {
            Ok(response) => {
                tracing::info!(conversation_id=%prompt.conversation_id,acp_session_id=%response.session_id,"OMP session created");
                return Ok(ready(response.session_id, response.config_options, emitter));
            }
            Err(error) if error.code == ErrorCode::AUTH_REQUIRED && !authenticated => {
                authenticated = true;
                connection
                    .send_request(AuthenticateRequest::new("agent"))
                    .block_task()
                    .await?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn ready(
    id: SessionId,
    options: Option<Vec<SessionConfigOption>>,
    emitter: &mut Emitter,
) -> Session {
    let options = serde_json::to_value(options.unwrap_or_default()).unwrap_or_default();
    let (model, thinking) = current_values(&options);
    emitter.emit(OmpWorkerEvent::SessionReady {
        acp_session_id: id.to_string(),
        config_options: options,
    });
    Session {
        id,
        model,
        thinking,
    }
}

pub(super) async fn configure(
    connection: &ConnectionTo<Agent>,
    session: &mut Session,
    prompt: &DOmpPrompt,
    emitter: &mut Emitter,
) {
    for (field, value) in [
        ("model", &prompt.model),
        ("thinking", &prompt.thinking_level),
    ] {
        let current = if field == "model" {
            &session.model
        } else {
            &session.thinking
        };
        if value.is_empty() || current.as_ref() == Some(value) {
            continue;
        }
        match connection
            .send_request(SetSessionConfigOptionRequest::new(
                session.id.clone(),
                field,
                value.as_str(),
            ))
            .block_task()
            .await
        {
            Ok(response) => {
                if let Ok(options) = serde_json::to_value(&response.config_options) {
                    (session.model, session.thinking) = current_values(&options);
                    emitter.emit(OmpWorkerEvent::Update {update:serde_json::json!({"sessionUpdate":"config_option_update","configOptions":options})});
                }
            }
            Err(_) => emitter.notice(
                "warning",
                "Setting not applied",
                &format!("OMP on this machine rejected {field} \"{value}\"."),
            ),
        }
    }
}

pub(super) fn update_config(session: &mut Session, update: &SessionUpdate) {
    if let SessionUpdate::ConfigOptionUpdate(update) = update
        && let Ok(options) = serde_json::to_value(&update.config_options)
    {
        (session.model, session.thinking) = current_values(&options);
    }
}

fn current_values(options: &serde_json::Value) -> (Option<String>, Option<String>) {
    let current = |id| {
        options
            .as_array()
            .and_then(|options| {
                options
                    .iter()
                    .find(|option| option["id"].as_str() == Some(id))
            })
            .and_then(|option| option["currentValue"].as_str())
            .map(str::to_owned)
    };
    (current("model"), current("thinking"))
}

fn discard_replay(
    events: &mut mpsc::UnboundedReceiver<super::actor::AcpEvent>,
    emitter: &mut Emitter,
) {
    while let Ok(event) = events.try_recv() {
        if let super::actor::AcpEvent::Notice(notice) = event {
            emitter.emit(notice);
        }
    }
}
