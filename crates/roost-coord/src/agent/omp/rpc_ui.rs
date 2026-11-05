//! Device-authorized semantic UI actions for persisted OMP conversations.
//! Each request is bound to the authenticated device tab, the mount view and
//! the current worker-generated transport before the owning worker sees it.
//! Bounded actions are admitted once; effect values never reach this path.

use super::host_service_store::{self as host_store, UiAdmission};
use super::host_services::{self, HostReply};
use super::{rpc, store};
use crate::{
    auth::principal::require_account_device, coord_core::CoordCore, rpc::service::ok_response,
};
use connectrpc::{ConnectError, ErrorCode, ServiceResult};
use roost_proto as proto;
use roost_protocol::wire::coord_worker::CoordWorkerDownstream;
use serde::Deserialize;
use serde_json::Value;

/// One action is a TSP message the native side already bounds at 2 MiB.
const MAX_ACTION_BYTES: usize =
    roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_MESSAGE_BYTES;
/// A bracketed paste must remain one atomic input, not per-view chunks.
const MAX_INPUT_BYTES: usize = roost_protocol::wire::agent_chat::semantic_ui::MAX_TSP_INPUT_BYTES;
const MAX_COMMAND_BYTES: usize = 4096;
const MAX_REQUEST_ID_BYTES: usize = 128;
const MAX_VIEW_ID_BYTES: usize = 128;
const MAX_TRANSPORT_ID_BYTES: usize = 256;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Action {
    method: String,
    params: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenParams {
    cols: u16,
    rows: u16,
    #[serde(rename = "dark")]
    _dark: bool,
    #[serde(rename = "reduceMotion")]
    _reduce_motion: bool,
    #[serde(default)]
    reset: bool,
    #[serde(default)]
    kinds: Vec<String>,
    #[serde(default)]
    features: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventParams {
    event: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputParams {
    data: String,
    #[serde(default)]
    before: Option<Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandParams {
    text: String,
}

/// The methods this RPC accepts, after their parameters were admitted.
enum ParsedAction {
    HostReply(HostReply),
    Open,
    Input,
    Event,
    Other,
}

impl ParsedAction {
    /// Only input and event actions are receipt-tracked by the native runtime.
    fn receipt_eligible(&self) -> bool {
        matches!(self, Self::Input | Self::Event)
    }
}

/// The authenticated origin and live token a request may act on.
struct BoundView {
    fingerprint: String,
    tab_id: String,
    view_id: String,
    transport_id: String,
}

pub async fn handle_agent_omp_ui(
    core: &CoordCore,
    caller: &crate::coord_core::Caller,
    request: proto::AgentOmpUiRequest,
) -> ServiceResult<proto::AgentOmpUiResponse> {
    require_account_device(caller)?;
    let action = parse_action(&request.action_json)?;
    let current = host_store::current_transport(&core.services.db, &request.conversation_id)
        .await
        .map_err(database_error)?;
    match action {
        ParsedAction::HostReply(reply) => {
            if request.request_id.is_empty() || request.request_id.len() > MAX_REQUEST_ID_BYTES {
                return Err(invalid("request id is empty or too long"));
            }
            if request.receipt {
                return Err(invalid("OMP host replies are not receipted"));
            }
            bind_view(caller, &request, &current, false)?;
            return host_services::reply(core, caller, &request, reply).await;
        }
        action => {
            if request.request_id.is_empty() || request.request_id.len() > MAX_REQUEST_ID_BYTES {
                return Err(invalid("request id is empty or too long"));
            }
            if request.receipt && !action.receipt_eligible() {
                return Err(invalid(
                    "receipts are supported only for input and event actions",
                ));
            }
            let opens = matches!(action, ParsedAction::Open);
            let view = bind_view(caller, &request, &current, opens)?;
            let receipt = request.receipt;
            return send_action(core, &request, view, receipt).await;
        }
    }
}

async fn send_action(
    core: &CoordCore,
    request: &proto::AgentOmpUiRequest,
    view: BoundView,
    receipt: bool,
) -> ServiceResult<proto::AgentOmpUiResponse> {
    let conversation = rpc::find_omp(core, &request.conversation_id)
        .await?
        .ok_or_else(|| ConnectError::new(ErrorCode::NotFound, "OMP conversation not found"))?;
    let worker = core
        .services
        .omp
        .worker_for_session(&conversation.worker_fp)
        .map_err(rpc::unavailable)?;
    let client_id = format!("{}:{}", view.fingerprint, view.view_id);
    let admission = UiAdmission {
        fingerprint: view.fingerprint.clone(),
        client_id: client_id.clone(),
        tab_id: view.tab_id.clone(),
        view_id: view.view_id.clone(),
        transport_id: view.transport_id.clone(),
        worker_fp: conversation.worker_fp.clone(),
        receipt,
        receipt_consumed: false,
    };
    if !host_store::admit_ui_action(
        &core.services.db,
        &conversation.id,
        &request.request_id,
        &admission,
        crate::serve::now_ms(),
    )
    .await
    .map_err(database_error)?
    {
        if receipt {
            host_services::publish_duplicate_receipt(
                core,
                &view.fingerprint,
                &view.tab_id,
                &conversation.id,
                &request.request_id,
                &view.view_id,
                &view.transport_id,
            )
            .await;
        }
        return ok_response(proto::AgentOmpUiResponse::default());
    }
    let frame = CoordWorkerDownstream::OmpUi(proto::DOmpUi {
        conversation_id: conversation.id.clone(),
        request_id: request.request_id.clone(),
        action_json: request.action_json.clone(),
        cwd: conversation.cwd,
        acp_session_id: conversation.acp_session_id.unwrap_or_default(),
        client_id,
        view_id: view.view_id,
        tab_id: view.tab_id,
        transport_id: view.transport_id,
        receipt,
        ..Default::default()
    });
    if worker.send(frame) == 0 {
        store::remove_admission(&core.services.db, &conversation.id, &request.request_id)
            .await
            .map_err(database_error)?;
        return Err(rpc::unavailable(super::OmpUnavailable::Offline));
    }
    ok_response(proto::AgentOmpUiResponse::default())
}

/// Bind the request to the authenticated tab and the live native transport.
///
/// Only a first `open` may carry an empty token; every other action must name
/// the current token, so a stale or cross-view action is refused before it
/// reaches the worker.
fn bind_view(
    caller: &crate::coord_core::Caller,
    request: &proto::AgentOmpUiRequest,
    current: &str,
    allows_first_open: bool,
) -> Result<BoundView, ConnectError> {
    let tab_id = caller
        .tab_id
        .as_deref()
        .filter(|tab| !tab.is_empty() && tab.len() <= 256)
        .ok_or_else(|| invalid("OMP UI requires an authenticated browser tab"))?;
    if request.tab_id != tab_id {
        return Err(invalid(
            "OMP UI tab does not match the authenticated socket",
        ));
    }
    if request.view_id.is_empty()
        || request.view_id.len() > MAX_VIEW_ID_BYTES
        || !request
            .view_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(invalid("OMP UI view id is empty or invalid"));
    }
    if request.transport_id.len() > MAX_TRANSPORT_ID_BYTES
        || request
            .transport_id
            .bytes()
            .any(|byte| byte.is_ascii_control())
    {
        return Err(invalid("OMP UI transport id is invalid"));
    }
    if request.transport_id.is_empty() {
        if !allows_first_open || !current.is_empty() {
            return Err(stale());
        }
    } else if request.transport_id != current {
        return Err(stale());
    }
    Ok(BoundView {
        fingerprint: caller.fingerprint().to_owned(),
        tab_id: tab_id.to_owned(),
        view_id: request.view_id.clone(),
        transport_id: request.transport_id.clone(),
    })
}

fn stale() -> ConnectError {
    ConnectError::new(
        ErrorCode::FailedPrecondition,
        "OMP UI view is stale; reopen it",
    )
}

fn parse_action(json: &str) -> Result<ParsedAction, ConnectError> {
    if json.is_empty() || json.len() > MAX_ACTION_BYTES {
        return Err(invalid("OMP UI action is empty or exceeds the size limit"));
    }
    let action: Action = serde_json::from_str(json)
        .map_err(|error| invalid(&format!("invalid OMP UI action: {error}")))?;
    let Action { method, params } = action;
    match method.as_str() {
        "host_reply" => Ok(ParsedAction::HostReply(HostReply::parse(params)?)),
        "open" => {
            let params: OpenParams = parse_params(params)?;
            let _ = params.reset;
            if !(1..=500).contains(&params.cols)
                || !(1..=300).contains(&params.rows)
                || !bounded_names(&params.kinds)
                || !bounded_names(&params.features)
            {
                return Err(invalid("invalid OMP UI open parameters"));
            }
            Ok(ParsedAction::Open)
        }
        "event" => {
            let params: EventParams = parse_params(params)?;
            if !params.event.is_object() || !safe_remote_value(&params.event, 0) {
                return Err(invalid("invalid OMP UI event"));
            }
            Ok(ParsedAction::Event)
        }
        "input" => {
            let params: InputParams = parse_params(params)?;
            if params.data.len() > MAX_INPUT_BYTES {
                return Err(invalid("OMP UI input exceeds the size limit"));
            }
            if let Some(before) = params.before
                && (!before.is_object()
                    || before.get("ev").and_then(Value::as_str) != Some("edit")
                    || !safe_remote_value(&before, 0))
            {
                return Err(invalid("OMP UI input pre-event must be a safe edit event"));
            }
            Ok(ParsedAction::Input)
        }
        "command" => {
            let params: CommandParams = parse_params(params)?;
            if params.text.len() > MAX_COMMAND_BYTES {
                return Err(invalid("OMP UI command exceeds the size limit"));
            }
            Ok(ParsedAction::Other)
        }
        "close" if params.as_object().is_some_and(serde_json::Map::is_empty) => {
            Ok(ParsedAction::Other)
        }
        "close" => Err(invalid("OMP UI close takes no parameters")),
        _ => Err(invalid("unsupported OMP UI action method")),
    }
}

fn parse_params<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, ConnectError> {
    serde_json::from_value(value)
        .map_err(|error| invalid(&format!("invalid OMP UI parameters: {error}")))
}

fn bounded_names(names: &[String]) -> bool {
    names.len() <= 128
        && names
            .iter()
            .all(|name| !name.is_empty() && name.len() <= 128)
}

fn safe_remote_value(value: &Value, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .all(|value| safe_remote_value(value, depth + 1)),
        Value::Object(values) => values.iter().all(|(key, value)| {
            !matches!(key.as_str(), "sessionId" | "session_id" | "cwd")
                && safe_remote_value(value, depth + 1)
        }),
        _ => true,
    }
}

fn invalid(message: &str) -> ConnectError {
    ConnectError::new(ErrorCode::InvalidArgument, message)
}

fn database_error(error: sqlx::Error) -> ConnectError {
    ConnectError::new(ErrorCode::Internal, error.to_string())
}

pub const METHOD_HANDLERS: &[(&str, &str)] =
    &[("AgentOmpUi", "agent::omp::rpc_ui::handle_agent_omp_ui")];
#[cfg(test)]
#[path = "rpc_ui/tests.rs"]
mod tests;
