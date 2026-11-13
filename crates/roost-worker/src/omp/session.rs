//! ACP initialization, history-suppressed session loading and prompt settings.
//! Called inside a supervised connection; OMP keeps its own local tools/login.
//! A failed load starts a new session with an explicit context-loss notice.

use super::{
    actor_types::AcpEvent,
    emitter::Emitter,
    interaction_pending::{self, PendingInteractions},
    launcher::CatalogReadiness,
    open_request::OpenRequestPump,
};
use agent_client_protocol::{
    Agent, Error, ErrorCode,
    v1::{ConnectionTo, schema::*},
};
use roost_proto::DOmpPrompt;
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use tokio::sync::mpsc;

/// The client declares the semantic command UI it renders so the fork gives its sessions the
/// transcript-rendering prompt blocks and `out://` replies the terminal transcript has.
fn command_ui_client_meta() -> Meta {
    let mut meta = Meta::new();
    meta.insert(
        "commandUi".into(),
        serde_json::json!({"version": 1, "protocol": "tsp"}),
    );
    meta
}

pub(super) struct Session {
    pub id: SessionId,
    pub resume_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub thinking: Option<String>,
    pub mode: Option<String>,
    pub available_modes: Vec<roost_protocol::wire::agent_chat::OmpChoice>,
}

pub(super) async fn open(
    connection: &ConnectionTo<Agent>,
    prompt: &DOmpPrompt,
    mut startup: OpenRequestPump<'_>,
    catalog: Option<&CatalogReadiness>,
) -> Result<Session, Error> {
    let initialized = startup
        .service(
            connection
                .send_request(
                    InitializeRequest::new(agent_client_protocol::schema::ProtocolVersion::V1)
                        .client_capabilities(
                            ClientCapabilities::new()
                                .elicitation(
                                    ElicitationCapabilities::new()
                                        .form(ElicitationFormCapabilities::new()),
                                )
                                .meta(command_ui_client_meta()),
                        ),
                )
                .block_task(),
        )
        .await?;
    let command_ui = initialized
        .agent_capabilities
        .meta
        .as_ref()
        .and_then(|metadata| metadata.get("commandUi"));
    if !command_ui.is_some_and(|capability| {
        capability
            .get("version")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
            && capability
                .get("protocol")
                .and_then(serde_json::Value::as_str)
                == Some("tsp")
    }) {
        return Err(Error::new(
            -32000,
            "This worker requires the OMP fork with semantic command UI version 1 (tsp). Install the build documented in deploy/omp/README.md.",
        ));
    }
    let result = open_authenticated(connection, prompt, &mut startup, catalog).await;
    if result
        .as_ref()
        .is_err_and(|error| error.code == ErrorCode::AUTH_REQUIRED)
    {
        startup.emitter.notice(
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
    startup: &mut OpenRequestPump<'_>,
    catalog: Option<&CatalogReadiness>,
) -> Result<Session, Error> {
    let mut authenticated = false;
    if !prompt.acp_session_id.is_empty() {
        loop {
            let loaded = startup
                .service(
                    connection
                        .send_request(LoadSessionRequest::new(
                            prompt.acp_session_id.clone(),
                            prompt.cwd.clone(),
                        ))
                        .block_task(),
                )
                .await;
            match loaded {
                Ok(response) => {
                    discard_replay(startup.events, startup.pending, startup.emitter);
                    tracing::info!(conversation_id=%prompt.conversation_id,acp_session_id=%prompt.acp_session_id,"OMP session loaded");
                    return ready(
                        prompt.acp_session_id.clone().into(),
                        &prompt.cwd,
                        response.config_options,
                        response.modes,
                        startup.emitter,
                        catalog,
                    );
                }
                Err(error) if error.code == ErrorCode::AUTH_REQUIRED && !authenticated => {
                    authenticated = true;
                    startup
                        .service(
                            connection
                                .send_request(AuthenticateRequest::new("agent"))
                                .block_task(),
                        )
                        .await?;
                }
                Err(error) if error.code == ErrorCode::AUTH_REQUIRED => return Err(error),
                Err(error) if error.code == ErrorCode::from(-32800) => return Err(error),
                Err(_) => {
                    discard_replay(startup.events, startup.pending, startup.emitter);
                    startup.emitter.notice("warning","Started a new OMP session","OMP could not reopen the previous session, so earlier messages are not in its context.");
                    break;
                }
            }
        }
    }
    loop {
        let created = startup
            .service(
                connection
                    .send_request(NewSessionRequest::new(prompt.cwd.clone()))
                    .block_task(),
            )
            .await;
        match created {
            Ok(response) => {
                tracing::info!(conversation_id=%prompt.conversation_id,acp_session_id=%response.session_id,"OMP session created");
                return ready(
                    response.session_id,
                    &prompt.cwd,
                    response.config_options,
                    response.modes,
                    startup.emitter,
                    catalog,
                );
            }
            Err(error) if error.code == ErrorCode::AUTH_REQUIRED && !authenticated => {
                authenticated = true;
                startup
                    .service(
                        connection
                            .send_request(AuthenticateRequest::new("agent"))
                            .block_task(),
                    )
                    .await?;
            }
            Err(error) => return Err(error),
        }
    }
}

fn ready(
    id: SessionId,
    cwd: &str,
    options: Option<Vec<SessionConfigOption>>,
    mode_state: Option<SessionModeState>,
    emitter: &mut Emitter,
    catalog: Option<&CatalogReadiness>,
) -> Result<Session, Error> {
    if let Some(catalog) = catalog {
        catalog.verify()?;
    }
    let options = serde_json::to_value(options.unwrap_or_default()).unwrap_or_default();
    let (model, thinking) = current_values(&options);
    let (mode, available_modes) = mode_state.map_or_else(
        || (None, Vec::new()),
        |state| {
            let mode = Some(state.current_mode_id.to_string());
            let modes = state
                .available_modes
                .into_iter()
                .map(|mode| roost_protocol::wire::agent_chat::OmpChoice {
                    value: mode.id.to_string(),
                    name: mode.name,
                    description: mode.description,
                })
                .collect();
            (mode, modes)
        },
    );
    emitter.emit(OmpWorkerEvent::Modes {
        modes: available_modes.clone(),
    });
    emitter.emit(OmpWorkerEvent::SessionReady {
        acp_session_id: id.to_string(),
        config_options: options,
        mode: mode.clone(),
    });
    Ok(Session {
        resume_id: id.to_string(),
        cwd: cwd.to_owned(),
        id,
        model,
        thinking,
        mode,
        available_modes,
    })
}

pub(super) async fn set_mode(
    connection: &ConnectionTo<Agent>,
    session: &mut Session,
    mode: String,
    emitter: &mut Emitter,
) -> bool {
    if mode.is_empty() || session.mode.as_deref() == Some(mode.as_str()) {
        return true;
    }
    if !session
        .available_modes
        .iter()
        .any(|choice| choice.value == mode)
    {
        emitter.notice(
            "warning",
            "Mode not applied",
            &format!("OMP did not offer mode \"{mode}\" for this session."),
        );
        return false;
    }
    match connection
        .send_request(SetSessionModeRequest::new(session.id.clone(), mode.clone()))
        .block_task()
        .await
    {
        Ok(_) => {
            tracing::info!(session_id=%session.id,mode,"OMP mode change accepted");
            true
        }
        Err(error) => {
            emitter.notice(
                "warning",
                "Mode not applied",
                &format!("OMP on this machine rejected mode \"{mode}\": {error}"),
            );
            false
        }
    }
}
pub(super) async fn configure(
    connection: &ConnectionTo<Agent>,
    session: &mut Session,
    prompt: &DOmpPrompt,
    emitter: &mut Emitter,
) -> bool {
    if !prompt.mode.is_empty() && session.mode.as_deref() == Some(prompt.mode.as_str()) {
        emitter.notice(
            "warning",
            "Plan prompt not submitted",
            "OMP is already in the requested mode; no prompt was submitted.",
        );
        return false;
    }
    if !prompt.mode.is_empty() && !set_mode(connection, session, prompt.mode.clone(), emitter).await
    {
        return false;
    }
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
    true
}

pub(super) fn update_config(session: &mut Session, update: &SessionUpdate) {
    match update {
        SessionUpdate::ConfigOptionUpdate(update) => {
            if let Ok(options) = serde_json::to_value(&update.config_options) {
                (session.model, session.thinking) = current_values(&options);
            }
        }
        SessionUpdate::CurrentModeUpdate(update) => {
            session.mode = Some(update.current_mode_id.to_string());
        }
        _ => {}
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
    events: &mut mpsc::UnboundedReceiver<AcpEvent>,
    pending: &mut PendingInteractions,
    emitter: &mut Emitter,
) {
    while let Ok(event) = events.try_recv() {
        interaction_pending::consume_open_event(event, pending, emitter);
    }
}
