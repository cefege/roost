//! Backend-specific RPC commands for worker-hosted OMP conversations.
//! Native handlers dispatch here only after device and request-id admission.
//! Prompt events persist before sending; the worker owns its prompt queue.

pub use super::rpc_respond::handle_agent_omp_respond;
pub use super::rpc_session::handle_agent_omp_session_open;
use super::{
    rpc_settings::database_error,
    store::{self, OmpConversation},
    summary::summary,
};
use crate::agent::sink::ChatSink;
use crate::{coord_core::CoordCore, rpc::service::ok_response};
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::{agent_chat::*, coord_worker::CoordWorkerDownstream};

pub(crate) async fn find_omp(
    core: &CoordCore,
    id: &str,
) -> Result<Option<OmpConversation>, ConnectError> {
    store::find(&core.services.db, id)
        .await
        .map_err(database_error)
}
pub(crate) fn unavailable(error: super::OmpUnavailable) -> ConnectError {
    match error {
        super::OmpUnavailable::Offline => {
            ConnectError::new(ErrorCode::Unavailable, "worker offline")
        }
        super::OmpUnavailable::UpdateRequired => ConnectError::new(
            ErrorCode::FailedPrecondition,
            "Update Roost on this machine to use OMP chats",
        ),
    }
}
fn writer_error(error: String) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, error)
}

pub(crate) async fn create(
    core: &CoordCore,
    request: proto::AgentChatCreateRequest,
    worker: (String, String, String),
) -> ServiceResult<proto::AgentConversation> {
    if !request.model_provider.is_empty() || !request.model_id.is_empty() {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "OMP conversations choose their model in settings",
        ));
    }
    core.services
        .omp
        .worker_for_session(&worker.0)
        .map_err(unavailable)?;
    let conversation = store::create(
        &core.services.db,
        store::OmpNew {
            worker_fp: worker.0,
            worker_label: worker.1,
            worker_os: worker.2,
            cwd: request.cwd,
        },
    )
    .await
    .map_err(database_error)?;
    publish(core, &conversation).await
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[
    (
        "AgentOmpSessionOpen",
        "agent::omp::rpc::handle_agent_omp_session_open",
    ),
    (
        "AgentOmpRespond",
        "agent::omp::rpc::handle_agent_omp_respond",
    ),
];

pub(crate) async fn submit(
    core: &CoordCore,
    conversation: OmpConversation,
    request: proto::AgentChatSubmitRequest,
) -> ServiceResult<proto::AgentChatSubmitResponse> {
    if store::has_prompt_admission(&core.services.db, &conversation.id, &request.request_id)
        .await
        .map_err(database_error)?
    {
        return ok_response(proto::AgentChatSubmitResponse::default());
    }
    if request.when_busy == "reject" && conversation.run_state == AgentRunState::Running {
        return Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            "OMP is busy",
        ));
    }
    let (text, mode) = if request.text.trim() == "/plan" {
        return Err(ConnectError::new(
            ErrorCode::InvalidArgument,
            "bare /plan uses the live mode control and must not submit a prompt",
        ));
    } else if let Some(suffix) = request
        .text
        .strip_prefix("/plan")
        .filter(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
    {
        let text = suffix.trim_start();
        if text.is_empty() {
            return Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "use /plan with a prompt or change mode without submitting",
            ));
        }
        if conversation.mode.as_deref() == Some("plan") {
            return Err(ConnectError::new(
                ErrorCode::FailedPrecondition,
                "OMP is already in plan mode; no prompt was submitted",
            ));
        }
        if conversation.run_state == AgentRunState::Running {
            return Err(ConnectError::new(
                ErrorCode::FailedPrecondition,
                "OMP plan prompt requires an idle session; no prompt was submitted",
            ));
        }
        core.services
            .omp
            .interaction_worker(&conversation.worker_fp)
            .map_err(unavailable)?;
        let offered = store::catalogs(&core.services.db)
            .await
            .map_err(database_error)?
            .into_iter()
            .find(|catalog| catalog.worker_fp == conversation.worker_fp)
            .is_some_and(|catalog| catalog.modes.iter().any(|choice| choice.value == "plan"));
        if !offered {
            return Err(ConnectError::new(
                ErrorCode::FailedPrecondition,
                "OMP did not offer plan mode; no prompt was submitted",
            ));
        }
        (text.to_owned(), "plan".into())
    } else {
        (request.text, String::new())
    };
    let mut images = vec![];
    let mut file_paths = vec![];
    let mut attachment_names = vec![];
    let mut image_bytes = 0usize;
    for attachment in request.attachments {
        attachment_names.push(attachment.filename);
        if attachment.mime_type.starts_with("image/") {
            image_bytes = image_bytes.saturating_add(attachment.data.len());
            if attachment.data.is_empty() || image_bytes > 4 * 1024 * 1024 {
                return Err(ConnectError::new(
                    ErrorCode::InvalidArgument,
                    "OMP image attachments are limited to 4 MiB per message",
                ));
            }
            images.push(proto::DOmpImage {
                mime_type: attachment.mime_type,
                data: attachment.data,
                ..Default::default()
            });
        } else if attachment.path.is_empty() {
            return Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "file attachment requires a worker path",
            ));
        } else {
            file_paths.push(attachment.path);
        }
    }
    let service = &core.services.omp;
    let worker = service
        .worker(&conversation.worker_fp)
        .map_err(unavailable)?;
    let effective = resolve_omp_settings(
        &store::settings(&service.db).await.map_err(database_error)?,
        &conversation.worker_fp,
        &conversation.cwd,
        &conversation.override_values,
    );
    let prompt = proto::DOmpPrompt {
        conversation_id: conversation.id.clone(),
        request_id: request.request_id,
        cwd: conversation.cwd,
        acp_session_id: conversation.acp_session_id.unwrap_or_default(),
        text,
        mode,
        images,
        file_paths,
        model: effective.model.value.unwrap_or_default(),
        thinking_level: effective.thinking_level.value.unwrap_or_default(),
        ..Default::default()
    };
    service
        .writers
        .submit(
            &conversation.id,
            super::writer::OmpInput {
                prompt,
                attachment_names,
                worker,
                reject_busy: request.when_busy == "reject",
            },
        )
        .await
        .map_err(|error| {
            let code = match error {
                super::writer::WriteError::Busy => ErrorCode::FailedPrecondition,
                super::writer::WriteError::Offline => ErrorCode::Unavailable,
                _ => ErrorCode::Internal,
            };
            ConnectError::new(code, error.to_string())
        })?;
    ok_response(proto::AgentChatSubmitResponse::default())
}

pub(crate) async fn abort(
    core: &CoordCore,
    conversation: OmpConversation,
    request: proto::AgentChatAbortRequest,
) -> ServiceResult<proto::AgentChatAbortResponse> {
    if store::admit_input(
        &core.services.db,
        &conversation.id,
        &request.request_id,
        "abort",
    )
    .await
    .map_err(database_error)?
    {
        core.services
            .omp
            .send_to_worker(
                &conversation.worker_fp,
                CoordWorkerDownstream::OmpCancel(proto::DOmpCancel {
                    conversation_id: conversation.id,
                    ..Default::default()
                }),
            )
            .map_err(unavailable)?;
    }
    ok_response(proto::AgentChatAbortResponse::default())
}

pub(crate) async fn configure(
    core: &CoordCore,
    conversation: OmpConversation,
    request: proto::AgentChatConfigureRequest,
) -> ServiceResult<proto::AgentConversation> {
    if request.worker_fp.is_some() || request.cwd.is_some() || request.read_only.is_some() {
        return Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            "OMP conversations cannot change machine, folder, or read-only mode",
        ));
    }
    let mode = request.mode.filter(|mode| !mode.is_empty());
    if let Some(mode) = &mode {
        core.services
            .omp
            .interaction_worker(&conversation.worker_fp)
            .map_err(unavailable)?;
        let offered = store::catalogs(&core.services.db)
            .await
            .map_err(database_error)?
            .into_iter()
            .find(|catalog| catalog.worker_fp == conversation.worker_fp)
            .is_some_and(|catalog| catalog.modes.iter().any(|choice| choice.value == *mode));
        if !offered {
            return Err(ConnectError::new(
                ErrorCode::InvalidArgument,
                "mode was not offered by this worker's ACP session",
            ));
        }
    }
    let model = match super::super::rpc_chat::model_update(
        request.model_provider.as_deref(),
        request.model_id.as_deref(),
    )? {
        super::super::rpc_chat::ModelUpdate::Unchanged => None,
        super::super::rpc_chat::ModelUpdate::Clear => Some(None),
        super::super::rpc_chat::ModelUpdate::Set(model) => Some(Some(join_omp_model(&model))),
    };
    store::set_override(
        &core.services.db,
        &conversation.id,
        model,
        request
            .thinking_level
            .map(|value| (!value.is_empty()).then_some(value)),
        None,
    )
    .await
    .map_err(database_error)?;
    if let Some(title) = request.title {
        store::set_title(&core.services.db, &conversation.id, &title)
            .await
            .map_err(database_error)?;
    }
    if let Some(mode) = mode {
        core.services
            .omp
            .send_to_worker(
                &conversation.worker_fp,
                CoordWorkerDownstream::OmpSetMode(proto::DOmpSetMode {
                    conversation_id: conversation.id.clone(),
                    request_id: format!("mode-{mode}"),
                    mode,
                    cwd: conversation.cwd.clone(),
                    acp_session_id: conversation.acp_session_id.clone().unwrap_or_default(),
                    ..Default::default()
                }),
            )
            .map_err(unavailable)?;
    }
    let updated = find_omp(core, &conversation.id)
        .await?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "conversation not found"))?;
    publish(core, &updated).await
}

pub(crate) async fn delete(
    core: &CoordCore,
    conversation: OmpConversation,
) -> ServiceResult<proto::AgentChatDeleteResponse> {
    let _ = core.services.omp.send_to_worker(
        &conversation.worker_fp,
        CoordWorkerDownstream::OmpClose(proto::DOmpClose {
            conversation_id: conversation.id.clone(),
            ..Default::default()
        }),
    );
    store::delete(&core.services.db, &conversation.id)
        .await
        .map_err(database_error)?;
    core.services.agent.sink.removed(&conversation.id).await;
    ok_response(proto::AgentChatDeleteResponse::default())
}

pub(crate) async fn snapshot(
    core: &CoordCore,
    conversation: OmpConversation,
) -> ServiceResult<proto::AgentChatSnapshotResponse> {
    let (seq, transcript) = store::snapshot(&core.services.db, &conversation.id)
        .await
        .map_err(database_error)?;
    ok_response(proto::AgentChatSnapshotResponse {
        seq,
        transcript_json: serde_json::to_string(&transcript)
            .map_err(|error| writer_error(error.to_string()))?,
        ..Default::default()
    })
}

async fn publish(
    core: &CoordCore,
    conversation: &OmpConversation,
) -> ServiceResult<proto::AgentConversation> {
    let summary = summary(
        conversation,
        &store::settings(&core.services.db)
            .await
            .map_err(database_error)?,
    );
    core.services.agent.sink.conversation(&summary).await;
    ok_response(conversation_to_proto(&summary))
}
