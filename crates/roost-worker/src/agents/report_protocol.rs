//! The one-request local protocol an installed integration speaks: volatile
//! `agent.report`, authenticated by a per-session capability. Ports
//! `apps/worker/src/agents/report-protocol.ts` with zod's first-issue wording,
//! because a refusal's `detail` is the integration author's only feedback
//! channel. Called by `agents::report_server` once per request line; depends
//! on `roost_protocol`.

use roost_protocol::fingerprint::is_fingerprint_hex;
use roost_protocol::wire::agent_status::{AGENT_STATUS_MESSAGE_MAX_LENGTH, AgentRuntimeState};
use roost_protocol::wire::brand::SessionId;
use serde_json::{Map, Value};

/// The largest request line the protocol accepts.
///
/// The endpoint is a loopback server any local process can reach, so the bound
/// is on the LINE rather than on the parsed message: a request that never
/// completes costs a connection, not a buffer.
pub const AGENT_REPORT_MAX_LINE_BYTES: usize = 32 * 1024;

/// Reported state is three-valued: there is no wire state for "no longer
/// known" (`done` is derived per viewer), so the withdrawal verb is named
/// instead of accepting herdr's fourth `unknown` value.
pub const REPORTED_STATE_UNKNOWN_REASON: &str =
    "state \"unknown\" is not reported; send active: false to withdraw the status";

const METHOD_REPORT: &str = "agent.report";

#[derive(Debug, Clone, PartialEq)]
pub struct AgentStatusReportRequest {
    pub capability: String,
    pub session_id: SessionId,
    pub state: AgentRuntimeState,
    pub message: Option<String>,
    pub active: bool,
}

/// Why a line is not a request. The server answers `invalid_json` or
/// `invalid_request` with `detail` as the first schema issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestRefusal {
    InvalidJson,
    InvalidRequest { detail: String },
}

type Issue = String;

/// Parse one request line.
pub fn parse_agent_integration_request(
    line: &str,
) -> Result<AgentStatusReportRequest, RequestRefusal> {
    let value: Value = serde_json::from_str(line).map_err(|_| RequestRefusal::InvalidJson)?;
    validate_request(&value).map_err(|detail| RequestRefusal::InvalidRequest { detail })
}

fn validate_request(value: &Value) -> Result<AgentStatusReportRequest, Issue> {
    let Value::Object(fields) = value else {
        return Err(type_issue("object", Some(value)));
    };
    if fields.get("method").and_then(Value::as_str) != Some(METHOD_REPORT) {
        return Err(format!(
            "Invalid discriminator value. Expected '{METHOD_REPORT}'"
        ));
    }
    if fields.get("version").and_then(Value::as_f64) != Some(1.0) {
        return Err("Invalid literal value, expected 1".to_owned());
    }
    let capability = expect_string(fields.get("capability"))?;
    if !is_fingerprint_hex(capability) {
        return Err("Invalid".to_owned());
    }
    let params = expect_object(fields.get("params"))?;
    let request = report_params(capability, params)?;
    refuse_unknown_keys(fields, &["version", "method", "capability", "params"])?;
    Ok(request)
}

fn report_params(
    capability: &str,
    params: &Map<String, Value>,
) -> Result<AgentStatusReportRequest, Issue> {
    let session_id = session_id(params.get("session_id"))?;
    let state = reported_state(params.get("state"))?;
    let message = match params.get("message") {
        None => None,
        Some(Value::String(message)) => {
            if message.encode_utf16().count() > AGENT_STATUS_MESSAGE_MAX_LENGTH {
                return Err(format!(
                    "String must contain at most {AGENT_STATUS_MESSAGE_MAX_LENGTH} character(s)"
                ));
            }
            Some(message.clone())
        }
        other => return Err(type_issue("string", other)),
    };
    let active = match params.get("active") {
        Some(Value::Bool(active)) => *active,
        other => return Err(type_issue("boolean", other)),
    };
    refuse_unknown_keys(params, &["session_id", "state", "message", "active"])?;
    Ok(AgentStatusReportRequest {
        capability: capability.to_owned(),
        session_id,
        state,
        message,
        active,
    })
}

fn session_id(value: Option<&Value>) -> Result<SessionId, Issue> {
    SessionId::try_from(expect_string(value)?).map_err(|_| "Invalid".to_owned())
}

fn reported_state(value: Option<&Value>) -> Result<AgentRuntimeState, Issue> {
    match expect_string(value)? {
        "working" => Ok(AgentRuntimeState::Working),
        "blocked" => Ok(AgentRuntimeState::Blocked),
        "idle" => Ok(AgentRuntimeState::Idle),
        "unknown" => Err(REPORTED_STATE_UNKNOWN_REASON.to_owned()),
        _ => Err("state must be one of working, blocked, idle".to_owned()),
    }
}

fn expect_string(value: Option<&Value>) -> Result<&str, Issue> {
    match value {
        Some(Value::String(text)) => Ok(text),
        other => Err(type_issue("string", other)),
    }
}

fn expect_object(value: Option<&Value>) -> Result<&Map<String, Value>, Issue> {
    match value {
        Some(Value::Object(fields)) => Ok(fields),
        other => Err(type_issue("object", other)),
    }
}

/// zod's `invalid_type` wording: a missing key is "Required".
fn type_issue(expected: &str, received: Option<&Value>) -> Issue {
    let received = match received {
        None => return "Required".to_owned(),
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    };
    format!("Expected {expected}, received {received}")
}

/// A strict object refuses keys its schema does not name. Reported after the
/// named keys, as zod does, so a bad named value is the first issue.
fn refuse_unknown_keys(fields: &Map<String, Value>, known: &[&str]) -> Result<(), Issue> {
    let unknown: Vec<String> = fields
        .keys()
        .filter(|key| !known.contains(&key.as_str()))
        .map(|key| format!("'{key}'"))
        .collect();
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Unrecognized key(s) in object: {}",
            unknown.join(", ")
        ))
    }
}
