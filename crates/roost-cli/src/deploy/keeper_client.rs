//! The one coordinator call a deploy and a keeper refresh make: the fenced
//! keeper-mutation boundary. Called by both commands; depends on the generated
//! Connect types in `roost-proto` and on the local coordinator database this
//! crate already reads for `roost status`, and on nothing else in the deploy
//! group.
//!
//! Two properties are load-bearing and both are about what is NOT here. There is
//! no hand-written method path: it is composed from the generated
//! `Spec` constant, so a rename in the `.proto` breaks the build instead of
//! producing a 404 at the moment somebody destroys a keeper. And the credential
//! is a parameter rather than something this module goes looking for, because
//! minting one is another slice's job and a module that quietly invented a
//! credential would be a second answer to "who is asking".
//!
//! The drain this call triggers lives in the coordinator: it stops
//! channel-creating commands, then asks the authenticated worker to perform one
//! journaled empty replacement or one deliberate maintenance. A deploy that
//! needed that drain to be safe would have to trust the ordering of two
//! processes; it is the coordinator's job, and this is the call that asks.

use std::path::Path;
use std::time::Duration;

use roost_proto::buffa::message::Message;
use roost_proto::{WorkersPrepareKeeperUpdateRequest, WorkersPrepareKeeperUpdateResponse};
use serde::{Deserialize, Serialize};

use crate::command_error::CommandFailure;
use crate::status::report::WorkerStatus;

/// The environment a deploy reads the coordinator's own address from. It is the
/// CLI's knob rather than a worker's, because it is the CLI that has to reach a
/// coordinator: a deploy run from a laptop is talking to a machine it is not
/// installing.
pub const COORD_URL_ENV: &str = "ROOST_COORD_URL";

/// The bearer credential this crate presents, when it has one. Another slice
/// mints it; this one only presents it.
pub const CLI_TOKEN_ENV: &str = "ROOST_CLI_TOKEN";

/// How long a keeper call may take before it is treated as unanswered. The
/// coordinator's drain has to finish talking to a worker that may be behind a
/// relayed hop, so this is generous; it is not a deadline on the drain, only on
/// this process's patience.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// What the coordinator reported about a keeper action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeeperActionOutcome {
    /// The coordinator's own word for what happened. Checked against the
    /// recorded action with `keeper_update_outcome_matches_action`, so an
    /// outcome this build does not recognise can never be read as permission.
    pub outcome: String,
    /// The keeper identity the coordinator settled on, which for a preserve is
    /// the value the convergence proof has to be rebased onto.
    pub keeper_pid: Option<i64>,
    pub keeper_epoch: Option<String>,
    pub binding_digest: Option<String>,
}

/// A reachable coordinator, addressed once and reused for the whole deploy.
#[derive(Debug, Clone)]
pub struct CoordinatorLink {
    origin: String,
    token: Option<String>,
    client: reqwest::Client,
}

impl CoordinatorLink {
    /// Address a coordinator. `origin` may carry a path prefix, which the
    /// method path is appended to — a coordinator behind a reverse proxy that
    /// mounts Roost under a sub-path is a supported install, and dropping the
    /// prefix would send every call to the proxy's own 404.
    pub fn new(origin: &str, token: Option<String>) -> Result<Self, CommandFailure> {
        let origin = origin.trim_end_matches('/').to_string();
        if origin.is_empty() {
            return Err(CommandFailure::generic(format!(
                "{COORD_URL_ENV} is empty; a keeper action needs a coordinator to fence it"
            )));
        }
        if !(origin.starts_with("http://") || origin.starts_with("https://")) {
            return Err(CommandFailure::generic(format!(
                "{COORD_URL_ENV}={origin} is not an http or https origin"
            )));
        }
        let client = reqwest::Client::builder()
            .timeout(CALL_TIMEOUT)
            .build()
            .map_err(|error| {
                CommandFailure::generic(format!("cannot build a coordinator client: {error}"))
            })?;
        Ok(Self {
            origin,
            token,
            client,
        })
    }

    /// The coordinator this link addresses, for a message.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Ask the coordinator to fence one keeper action.
    ///
    /// `journaled_update` is the recorded envelope for a deploy and is empty for
    /// a maintenance. `force_live` is passed explicitly and is never inferred
    /// from any other field: it is the operator's authorization to end live PTYs,
    /// and a value that could be implied by a neighbouring field would be
    /// re-authorized by accident.
    pub async fn prepare_keeper_action(
        &self,
        worker_fingerprint: &str,
        journaled_update: &str,
        direction: &str,
        maintenance: bool,
        force_live: bool,
    ) -> Result<KeeperActionOutcome, CommandFailure> {
        let request = WorkersPrepareKeeperUpdateRequest {
            worker_fp: worker_fingerprint.to_string(),
            journaled_update_json: if journaled_update.is_empty() {
                None
            } else {
                Some(journaled_update.to_string())
            },
            direction: direction.to_string(),
            maintenance,
            force_live,
            ..Default::default()
        };
        let response: WorkersPrepareKeeperUpdateResponse = self.call(&request).await?;
        Ok(KeeperActionOutcome {
            outcome: response.outcome,
            keeper_pid: response.keeper_pid.map(|pid| pid as i64),
            keeper_epoch: response.keeper_epoch,
            binding_digest: response.binding_digest,
        })
    }

    /// One Connect unary call, with the method path taken from the generated
    /// spec rather than written out.
    async fn call<Request, Response>(&self, request: &Request) -> Result<Response, CommandFailure>
    where
        Request: Message,
        Response: Message,
    {
        let url = format!(
            "{}{}",
            self.origin,
            method_path(roost_proto::COORDINATOR_SERVICE_WORKERS_PREPARE_KEEPER_UPDATE_SPEC)
        );
        let mut builder = self
            .client
            .post(&url)
            // Connect unary over HTTP: the body is the bare message and the
            // content type is the codec, with no envelope framing.
            .header("content-type", "application/proto")
            .body(request.encode_to_vec());
        if let Some(token) = &self.token {
            builder = builder.bearer_auth(token);
        }
        let response = builder.send().await.map_err(|error| {
            CommandFailure::generic(format!(
                "the coordinator at {} did not answer the keeper action: {error}",
                self.origin
            ))
        })?;
        let status = response.status();
        let body = response.bytes().await.map_err(|error| {
            CommandFailure::generic(format!(
                "the coordinator at {} closed the connection mid-answer: {error}",
                self.origin
            ))
        })?;
        if !status.is_success() {
            return Err(CommandFailure::generic(format!(
                "the coordinator at {} refused the keeper action with HTTP {}: {}",
                self.origin,
                status.as_u16(),
                String::from_utf8_lossy(&body).trim()
            )));
        }
        Response::decode_from_slice(&body).map_err(|error| {
            CommandFailure::generic(format!(
                "the coordinator at {} answered the keeper action in a shape this build cannot \
                 read: {error}",
                self.origin
            ))
        })
    }
}

/// The Connect procedure path for a generated method, composed from the spec's
/// own service and method names.
fn method_path(spec: connectrpc::Spec) -> String {
    format!("/{}/{}", spec.service(), spec.method())
}

/// The worker roster this box's coordinator has, which is the only evidence
/// available about a remote machine's keeper.
///
/// Read from the local coordinator database, read-only, exactly as `roost
/// status` reads it: the deploying box IS the coordinator's host in the
/// supported arrangement, and a second way to ask a coordinator for its roster
/// would be a second answer to "what does the fleet look like right now".
pub async fn worker_inventory(
    database_path: &Path,
    now_ms: i64,
) -> Result<Vec<WorkerStatus>, CommandFailure> {
    Ok(crate::status::inventory::worker_inventory(database_path, now_ms).await?)
}

/// The one worker row a host names, or the refusal that says why it is not one.
pub fn exactly_one_worker<'a>(
    inventory: &'a [WorkerStatus],
    host: &str,
) -> Result<&'a WorkerStatus, CommandFailure> {
    let matching: Vec<&WorkerStatus> = inventory
        .iter()
        .filter(|worker| crate::deploy::admission::worker_matches_target(worker, host))
        .collect();
    match matching.as_slice() {
        [worker] => Ok(worker),
        [] => Err(CommandFailure::generic(format!(
            "{host}: no registered worker matches this host"
        ))),
        _ => Err(CommandFailure::generic(format!(
            "{host}: this host matches {} registered workers, so nothing can be proven about it",
            matching.len()
        ))),
    }
}
