//! `roost api workspaces` and the four `ws-*` mutations. Called by `api::mod`;
//! depends on the generated `Workspaces*` methods and on `api::client`.
//!
//! EVERY MUTATION HERE IS VERSION-FENCED, NOT JUST `ws-update`. The
//! coordinator requires an exact `if_version` on all four mutating calls, so
//! reading the version once and writing it back is the only way any of them can
//! succeed at all. The read is a list, because the contract has no point-get
//! for a workspace and the list is small.
//!
//! WHY ONE RETRY, AND ONLY ON A VERSION CONFLICT. A browser that saved a
//! layout between this command's read and its write bumps the version, and one
//! retry absorbs that. Two things it must not do: retry any other refusal,
//! because the coordinator's word on those is final and re-running would
//! re-execute a command that already said no; and retry in a loop, because that
//! turns a coordinator being actively written into an unbounded write from a
//! terminal, with no way for the operator watching it to tell that fight from
//! progress.

use std::process::ExitCode;

use connectrpc::ErrorCode;
use roost_proto::{
    WorkspacesCreateRequest, WorkspacesDeleteRequest, WorkspacesListRequest,
    WorkspacesSetSessionsRequest, WorkspacesUpdateRequest,
};

use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// The workspace table, one row per workspace.
pub async fn list(
    api: &CoordinatorApi,
    _args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let response = api
        .answer(api.stub().workspaces_list(WorkspacesListRequest::default()))
        .await?;
    out.answer("id\tworker\tname\tfolder\tsessions");
    for workspace in &response.workspaces {
        let worker: String = workspace.worker_fp.chars().take(8).collect();
        out.answer(&format!(
            "{}\t{}\t{}\t{}\t{} sess",
            workspace.id,
            worker,
            workspace.name,
            workspace.folder_path,
            workspace.session_ids.len()
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// Create a workspace and print its new id, which is the only thing a caller
/// needs next.
pub async fn create(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let worker = args.positional(0, "worker")?;
    let name = args.positional(1, "name")?;
    let folder = args.positional(2, "folder")?;
    let mut response = api
        .answer(api.stub().workspaces_create(WorkspacesCreateRequest {
            worker_fp: worker.to_string(),
            name: name.to_string(),
            folder_path: folder.to_string(),
            color: args.optional_value("--color").map(str::to_string),
            ..Default::default()
        }))
        .await?;
    out.answer(
        &response
            .workspace
            .take()
            .map_or_else(String::new, |workspace| workspace.id),
    );
    Ok(ExitCode::SUCCESS)
}

/// Change a workspace's name, colour or position, and print its new version.
pub async fn update(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let id = args.positional(0, "id")?.to_string();
    let position = args
        .optional_value("--position")
        .map(|raw| {
            raw.parse::<u32>().map_err(|_| {
                CommandFailure::usage(format!(
                    "roost api ws-update: --position must be a non-negative whole number, got \
                     {raw:?}"
                ))
            })
        })
        .transpose()?;
    let name = args.optional_value("--name").map(str::to_string);
    let color = args.optional_value("--color").map(str::to_string);
    if name.is_none() && color.is_none() && position.is_none() {
        return Err(CommandFailure::usage(
            "roost api ws-update: give at least one of --name, --color, --position",
        ));
    }
    let mut attempt = WorkspacesUpdateRequest {
        id: id.clone(),
        if_version: version_of(api, &id).await?,
        name,
        color,
        position,
        ..Default::default()
    };
    let mut response = match api
        .try_answer(api.stub().workspaces_update(attempt.clone()))
        .await
    {
        Ok(response) => response,
        Err((_, ErrorCode::FailedPrecondition)) => {
            attempt.if_version = version_of(api, &id).await?;
            api.answer(api.stub().workspaces_update(attempt)).await?
        }
        Err((failure, _)) => return Err(failure),
    };
    out.answer(
        &response
            .workspace
            .take()
            .map_or_else(String::new, |workspace| workspace.version.to_string()),
    );
    Ok(ExitCode::SUCCESS)
}

/// Delete a workspace, printing whether the coordinator took it.
pub async fn delete(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let id = args.positional(0, "id")?.to_string();
    let mut attempt = WorkspacesDeleteRequest {
        id: id.clone(),
        if_version: version_of(api, &id).await?,
        ..Default::default()
    };
    let ok = match api
        .try_answer(api.stub().workspaces_delete(attempt.clone()))
        .await
    {
        Ok(response) => response.ok,
        Err((_, ErrorCode::FailedPrecondition)) => {
            attempt.if_version = version_of(api, &id).await?;
            api.answer(api.stub().workspaces_delete(attempt)).await?.ok
        }
        Err((failure, _)) => return Err(failure),
    };
    out.answer(&ok.to_string());
    Ok(ExitCode::SUCCESS)
}

/// Set a workspace's session membership exactly, and print its new version.
pub async fn set_sessions(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let id = args.positional(0, "id")?.to_string();
    let session_ids = args.positionals[1..].to_vec();
    let mut attempt = WorkspacesSetSessionsRequest {
        id: id.clone(),
        if_version: version_of(api, &id).await?,
        session_ids,
        ..Default::default()
    };
    let mut response = match api
        .try_answer(api.stub().workspaces_set_sessions(attempt.clone()))
        .await
    {
        Ok(response) => response,
        Err((_, ErrorCode::FailedPrecondition)) => {
            attempt.if_version = version_of(api, &id).await?;
            api.answer(api.stub().workspaces_set_sessions(attempt))
                .await?
        }
        Err((failure, _)) => return Err(failure),
    };
    out.answer(
        &response
            .workspace
            .take()
            .map_or_else(String::new, |workspace| workspace.version.to_string()),
    );
    Ok(ExitCode::SUCCESS)
}

/// A workspace's current version, read through the list because the contract
/// has no point-get for one.
async fn version_of(api: &CoordinatorApi, id: &str) -> Result<u64, CommandFailure> {
    let response = api
        .answer(api.stub().workspaces_list(WorkspacesListRequest::default()))
        .await?;
    response
        .workspaces
        .iter()
        .find(|workspace| workspace.id == id)
        .map(|workspace| workspace.version)
        .ok_or_else(|| {
            CommandFailure::usage(format!(
                "roost api: the coordinator lists no workspace {id}"
            ))
        })
}
