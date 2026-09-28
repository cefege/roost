//! `roost api workers`, `worker-rename` and `worker-rm`: the three verbs that
//! read the fleet roster and change what a machine is called or whether it is
//! enrolled. Called by `api::mod`; depends on the generated `Workers*`
//! methods and on `api::client`.
//!
//! WHY A NAME IS ENOUGH TO FIND A MACHINE. A worker fingerprint is 64 hex
//! characters. An operator reading `roost api workers` has a label in front of
//! them, and making them copy a fingerprint out of one column to paste into the
//! next command is a step where the wrong machine is chosen. So an exact
//! fingerprint still works, a unique prefix works, and an exact label works —
//! and an ambiguous one is refused with the candidates named, because "removed
//! the first match" is not an answer anybody asked for.

use std::process::ExitCode;

use roost_proto::{WorkersDeleteRequest, WorkersListRequest, WorkersRenameRequest};

use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// The roster, one row per machine, with routability rather than heartbeat
/// freshness as the "online" column.
pub async fn list(
    api: &CoordinatorApi,
    _args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let response = api
        .answer(api.stub().workers_list(WorkersListRequest::default()))
        .await?;
    out.answer("fp\tlabel\tstate\tos");
    for worker in &response.workers {
        let routable = response
            .routable_fps
            .iter()
            .any(|fingerprint| fingerprint == &worker.fp);
        out.answer(&format!(
            "{}\t{}\t{}\t{}",
            worker.fp,
            worker.label,
            if routable { "online" } else { "offline" },
            worker.os
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// Relabel a machine.
pub async fn rename(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let named = args.positional(0, "fp|prefix|label")?;
    let label = args.joined_from(1);
    if label.is_empty() {
        return Err(CommandFailure::usage(
            "roost api worker-rename: missing <label>",
        ));
    }
    let fingerprint = resolve(api, named).await?;
    let mut response = api
        .answer(api.stub().workers_rename(WorkersRenameRequest {
            fp: fingerprint,
            label,
            ..Default::default()
        }))
        .await?;
    out.answer(&match response.worker.take() {
        Some(worker) => worker.label,
        None => String::new(),
    });
    Ok(ExitCode::SUCCESS)
}

/// Deregister a machine: the `WorkersDelete` that drops the worker and its
/// authorized key together, which is the API-side equivalent of removing it in
/// Settings.
pub async fn remove(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let named = args.positional(0, "fp|prefix|label")?;
    let fingerprint = resolve(api, named).await?;
    let ok = api
        .answer(api.stub().workers_delete(WorkersDeleteRequest {
            fp: fingerprint,
            ..Default::default()
        }))
        .await?
        .ok;
    out.answer(&ok.to_string());
    Ok(ExitCode::SUCCESS)
}

/// The one fingerprint a name resolves to, or the refusal that says why not.
pub async fn resolve(api: &CoordinatorApi, named: &str) -> Result<String, CommandFailure> {
    let response = api
        .answer(api.stub().workers_list(WorkersListRequest::default()))
        .await?;
    if let Some(exact) = response
        .workers
        .iter()
        .find(|worker| worker.fp == named)
        .map(|worker| worker.fp.clone())
    {
        return Ok(exact);
    }
    let matches: Vec<&roost_proto::Worker> = response
        .workers
        .iter()
        .filter(|worker| worker.fp.starts_with(named) || worker.label == named)
        .collect();
    match matches.as_slice() {
        [] => Err(CommandFailure::usage(format!(
            "roost api: no worker matches {named:?}"
        ))),
        [only] => Ok(only.fp.clone()),
        many => {
            let candidates = many
                .iter()
                .map(|worker| {
                    let short: String = worker.fp.chars().take(8).collect();
                    format!("{}({short})", worker.label)
                })
                .collect::<Vec<_>>()
                .join(", ");
            Err(CommandFailure::usage(format!(
                "roost api: {named:?} is ambiguous — it matches {candidates}"
            )))
        }
    }
}
