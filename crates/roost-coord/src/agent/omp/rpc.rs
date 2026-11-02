//! Backend-specific RPC commands for worker-hosted OMP conversations.
//! Native handlers dispatch here only after device and request-id admission.
//! Prompt events persist before sending; the worker owns its prompt queue.

use super::{
    rpc_settings::database_error,
    store::{self, OmpConversation},
    summary::summary,
};
use crate::{coord_core::CoordCore, rpc::service::ok_response};
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_agent::ChatSink;
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

pub(crate) fn unsupported() -> ConnectError {
    ConnectError::new(
        ErrorCode::FailedPrecondition,
        "not supported for OMP conversations",
    )
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

pub async fn handle_agent_omp_session_open(
    core: &CoordCore,
    caller: &crate::coord_core::Caller,
    request: proto::AgentOmpSessionOpenRequest,
) -> ServiceResult<proto::AgentOmpSessionOpenResponse> {
    crate::auth::principal::require_account_device(caller)?;
    let conversation = find_omp(core, &request.conversation_id)
        .await?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "OMP conversation not found"))?;
    let worker = core
        .services
        .omp
        .worker_for_session(&conversation.worker_fp)
        .map_err(unavailable)?;
    if worker.send(CoordWorkerDownstream::OmpOpen(proto::DOmpOpen {
        conversation_id: conversation.id,
        cwd: conversation.cwd,
        acp_session_id: conversation.acp_session_id.unwrap_or_default(),
        ..Default::default()
    })) == 0
    {
        return Err(unavailable(super::OmpUnavailable::Offline));
    }
    ok_response(proto::AgentOmpSessionOpenResponse::default())
}

pub const METHOD_HANDLERS: &[(&str, &str)] = &[(
    "AgentOmpSessionOpen",
    "agent::omp::rpc::handle_agent_omp_session_open",
)];

pub(crate) async fn submit(
    core: &CoordCore,
    conversation: OmpConversation,
    request: proto::AgentChatSubmitRequest,
) -> ServiceResult<proto::AgentChatSubmitResponse> {
    if request.when_busy == "reject" && conversation.run_state == AgentRunState::Running {
        return Err(ConnectError::new(
            ErrorCode::FailedPrecondition,
            "OMP is busy",
        ));
    }
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
        text: request.text,
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
    )
    .await
    .map_err(database_error)?;
    if let Some(title) = request.title {
        store::set_title(&core.services.db, &conversation.id, &title)
            .await
            .map_err(database_error)?;
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
