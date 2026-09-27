//! What one observed agent status looks like on the way out, and what it looks
//! like on the way in. Called by `api::agents` and `api::agent_prompt`; depends
//! on the generated `AgentStatusView` and on `roost-protocol`'s agent-status
//! contract, and on nothing else in this crate.
//!
//! TWO DIRECTIONS, ONE FILE, ON PURPOSE. `project` is the outward shape — the
//! JSON document and the TSV row an operator or a script reads — and `fields`
//! is the inward one, turning the wire message into the `roost-protocol` type
//! the coordinator and the browser already order by. They live together because
//! they are the same value seen from both ends, and splitting them is how the
//! printed `state` and the ordered `state` drift into two answers.
//!
//! WHY `fields` IS NOT IN `roost-protocol::proto_adapters`. That is where it
//! belongs — the coordinator needs the same adapter for the same message, and
//! two copies of a proto conversion is exactly the fork this repository pays
//! for most. It is here because that module is not this slice's file; the lead
//! should move it and point both callers at the one.

use roost_proto::AgentStatusView;
use roost_protocol::wire::{
    AgentId, AgentOccupantId, AgentRuntimeState, AgentStatusFields, AgentStatusSource, SessionId,
    StatusEpoch,
};
use serde::Serialize;

use crate::command_error::CommandFailure;

/// The largest integer a JavaScript reader can hold without losing a digit.
///
/// A revision past it is a producer that was not counting in milliseconds, and
/// a `--json` document whose numbers round under the reader that consumes it is
/// worse than a refusal: the script sees a plausible number that is not the one
/// the coordinator sent.
pub const MAX_JSON_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// The TSV header `agents` and `agent-status` print above their rows.
pub const TSV_HEADER: &str = "session_id\tagent_id\tstate\tmessage\tstatus_epoch\toccupant_id\t\
                              source\trevision\tcompleted_revision\tupdated_at\tpromptable";

/// One observed agent status, in the shape this command has always published.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentStatusProjection {
    pub session_id: String,
    pub agent_id: String,
    pub state: String,
    pub message: Option<String>,
    pub status_epoch: Option<String>,
    pub occupant_id: Option<String>,
    pub source: Option<String>,
    pub revision: i64,
    pub completed_revision: i64,
    pub updated_at: f64,
    pub promptable: bool,
}

impl AgentStatusProjection {
    /// The one TSV row shape, with every cell escaped so a row stays one line.
    #[must_use]
    pub fn tsv_row(&self) -> String {
        let revision = self.revision.to_string();
        let completed = self.completed_revision.to_string();
        let updated = self.updated_at.to_string();
        let promptable = if self.promptable { "true" } else { "false" };
        let cells = [
            self.session_id.as_str(),
            self.agent_id.as_str(),
            self.state.as_str(),
            self.message.as_deref().unwrap_or("-"),
            self.status_epoch.as_deref().unwrap_or("-"),
            self.occupant_id.as_deref().unwrap_or("-"),
            self.source.as_deref().unwrap_or("legacy"),
            revision.as_str(),
            completed.as_str(),
            updated.as_str(),
            promptable,
        ];
        cells
            .iter()
            .map(|cell| escape_cell(cell))
            .collect::<Vec<_>>()
            .join("\t")
    }
}

/// The outward shape, refusing only what cannot be represented faithfully.
pub fn project(view: &AgentStatusView) -> Result<AgentStatusProjection, CommandFailure> {
    Ok(AgentStatusProjection {
        session_id: view.session_id.clone(),
        agent_id: view.agent_id.clone(),
        state: view.state.clone(),
        message: view.message.clone(),
        status_epoch: view.status_epoch.clone(),
        occupant_id: view.occupant_id.clone(),
        source: view.source.clone(),
        revision: json_safe(view.revision, "revision")?,
        completed_revision: json_safe(view.completed_revision, "completed_revision")?,
        updated_at: finite(view.updated_at, "updated_at")?,
        promptable: view.promptable,
    })
}

fn json_safe(value: u64, field: &str) -> Result<i64, CommandFailure> {
    let converted = i64::try_from(value).unwrap_or(i64::MAX);
    if converted > MAX_JSON_SAFE_INTEGER {
        return Err(CommandFailure::generic(format!(
            "agent status {field} exceeds the JSON integer range"
        )));
    }
    Ok(converted)
}

fn finite(value: f64, field: &str) -> Result<f64, CommandFailure> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(CommandFailure::generic(format!(
            "agent status {field} is not a finite number"
        )))
    }
}

/// One document on one line, for the one verb whose stdout a script parses.
pub fn json_line<T: Serialize>(value: &T) -> Result<String, CommandFailure> {
    serde_json::to_string_pretty(value)
        .map_err(|error| CommandFailure::generic(format!("agent status is not printable: {error}")))
}

/// Control characters, the delimiter's own escape, and nothing else. A status
/// message is free text a worker put on the screen, so a raw newline in one
/// would silently become a second row and a raw tab a second column.
fn escape_cell(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\t' => escaped.push_str("\\t"),
            '\r' => escaped.push_str("\\r"),
            '\n' => escaped.push_str("\\n"),
            other if other.is_control() => {
                escaped.push_str(&format!("\\x{:02x}", other as u32));
            }
            other => escaped.push(other),
        }
    }
    escaped
}

/// The runtime state a wire state names, when this build knows the name.
///
/// The vocabulary is closed — `roost-protocol` has three variants and the
/// coordinator validates through them — so a fourth name on the wire is a
/// contract violation rather than a state to pass through. Refusing is what
/// keeps `agent-wait` from reporting a state it cannot order.
#[must_use]
pub fn runtime_state(state: &str) -> Option<AgentRuntimeState> {
    match state {
        "working" => Some(AgentRuntimeState::Working),
        "blocked" => Some(AgentRuntimeState::Blocked),
        "idle" => Some(AgentRuntimeState::Idle),
        _ => None,
    }
}

/// The wire message as the type the coordinator and the browser order by.
pub fn fields(view: &AgentStatusView) -> Result<AgentStatusFields, CommandFailure> {
    let state = runtime_state(&view.state).ok_or_else(|| {
        CommandFailure::generic(format!(
            "agent status for {} reports the state {:?}, which is not one of blocked, idle, \
             working",
            view.session_id, view.state
        ))
    })?;
    Ok(AgentStatusFields {
        session_id: SessionId::try_from(view.session_id.clone()).map_err(protocol)?,
        agent_id: AgentId::try_from(view.agent_id.clone()).map_err(protocol)?,
        state,
        message: view.message.clone(),
        revision: i64::try_from(view.revision)
            .map_err(|_| too_large("revision", view.session_id.as_str()))?,
        completed_revision: i64::try_from(view.completed_revision)
            .map_err(|_| too_large("completed_revision", view.session_id.as_str()))?,
        updated_at: wall_millis(view.updated_at, view.session_id.as_str())?,
        status_epoch: view
            .status_epoch
            .as_deref()
            .map(StatusEpoch::try_from)
            .transpose()
            .map_err(protocol)?,
        occupant_id: view
            .occupant_id
            .as_deref()
            .map(AgentOccupantId::try_from)
            .transpose()
            .map_err(protocol)?,
        source: source(view.source.as_deref()),
        occupant_exited: false,
    })
}

fn source(raw: Option<&str>) -> Option<AgentStatusSource> {
    match raw? {
        "integration" => Some(AgentStatusSource::Integration),
        "screen" => Some(AgentStatusSource::Screen),
        _ => None,
    }
}

fn wall_millis(value: f64, session: &str) -> Result<i64, CommandFailure> {
    if !value.is_finite() {
        return Err(CommandFailure::generic(format!(
            "agent status for {session} reports a non-finite updated_at"
        )));
    }
    let truncated = value.trunc();
    if truncated < i64::MIN as f64 || truncated > i64::MAX as f64 {
        return Err(too_large("updated_at", session));
    }
    Ok(truncated as i64)
}

fn too_large(field: &str, session: &str) -> CommandFailure {
    CommandFailure::generic(format!(
        "agent status for {session} reports a {field} this build cannot represent"
    ))
}

fn protocol(error: roost_protocol::ProtocolError) -> CommandFailure {
    CommandFailure::generic(format!("agent status is not a valid one: {error}"))
}

#[cfg(test)]
mod tests {
    use super::{AgentStatusProjection, MAX_JSON_SAFE_INTEGER, escape_cell, finite, json_safe};
    use crate::command_error::CommandFailure;

    fn projection() -> AgentStatusProjection {
        AgentStatusProjection {
            session_id: "session-1".to_string(),
            agent_id: "omp".to_string(),
            state: "working".to_string(),
            message: None,
            status_epoch: None,
            occupant_id: None,
            source: None,
            revision: 7,
            completed_revision: 5,
            updated_at: 1.5,
            promptable: true,
        }
    }

    #[test]
    fn a_row_keeps_its_column_count_when_a_message_contains_delimiters() {
        let mut status = projection();
        status.message = Some("line one\nline\ttwo".to_string());
        let row = status.tsv_row();
        assert_eq!(row.split('\t').count(), 11, "{row}");
        assert!(!row.contains('\n'), "a raw newline survived into the row: {row}");
    }

    #[test]
    fn an_absent_optional_reads_as_a_dash_and_not_an_empty_cell() {
        let row = projection().tsv_row();
        let cells: Vec<&str> = row.split('\t').collect();
        assert_eq!(cells[3], "-");
        assert_eq!(cells[4], "-");
        assert_eq!(cells[6], "legacy");
        assert_eq!(cells[10], "true");
    }

    #[test]
    fn a_control_character_is_escaped_rather_than_printed() {
        assert_eq!(escape_cell("a\u{7}b"), "a\\x07b");
        assert_eq!(escape_cell("a\\b"), "a\\\\b");
    }

    #[test]
    fn a_revision_a_json_reader_would_round_is_refused_not_rounded() {
        let past = (MAX_JSON_SAFE_INTEGER + 1) as u64;
        assert!(json_safe(past, "revision").is_err());
        assert_eq!(
            json_safe(MAX_JSON_SAFE_INTEGER as u64, "revision"),
            Ok(MAX_JSON_SAFE_INTEGER)
        );
    }

    #[test]
    fn a_non_finite_timestamp_is_refused_rather_than_serialised_as_null() {
        let failure = finite(f64::NAN, "updated_at")
            .expect_err("NaN has no JSON number and no meaning as a wall clock");
        let CommandFailure { message, .. } = failure;
        assert!(message.contains("updated_at"), "{message}");
    }
}
