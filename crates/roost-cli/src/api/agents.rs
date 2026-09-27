//! `roost api agents`, `agent-status` and `agent-wait`: the three verbs that
//! read what a coordinator has observed about the agents in a session. Called
//! by `api::mod` after it has decided the verb and parsed the arguments;
//! depends on the generated `AgentStatus*` methods, on `roost-protocol`'s
//! agent-status ordering, and on `api::agent_projection` for the shape.
//!
//! WHY THE WAIT READS A SECOND TIME. The coordinator's wait answers with an
//! outcome word and no state, so the word alone cannot tell a caller what the
//! agent is doing. The second read is where the state comes from — and it is
//! read *after* the wait returned, so it is at least as fresh as the fence the
//! wait was parked on. What it is not is automatically about the same agent: an
//! occupant can be replaced while the wait is parked, and reporting the new
//! occupant's state as "the state it was still in" would be a lie about a
//! process the caller never watched. So the re-read is admitted only when
//! `roost-protocol`'s `AgentStatusOrder` and `same_agent_status_occupant` agree
//! it is the same occupant at a later revision — the one predicate the
//! coordinator and the browser already share, and the reason this module does
//! not carry a freshness rule of its own.

use std::process::ExitCode;

use roost_proto::{
    AgentStatusGetRequest, AgentStatusListRequest, AgentStatusView, AgentStatusWaitRequest,
};
use roost_protocol::terminal_input::{
    AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS, AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS,
    validate_agent_prompt_wait_timeout_ms,
};
use roost_protocol::wire::agent_status::{
    AgentStatusOrder, agent_status_identity, same_agent_status_occupant,
};
use roost_protocol::wire::{AgentStatus, AgentStatusFields, AgentStatusUpdate};

use crate::api::agent_projection::{self, TSV_HEADER};
use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::{CommandFailure, GENERIC_FAILURE};

/// The states `--until` may name. Closed, because `roost-protocol` owns the
/// vocabulary and a wait for a state it does not have could never be satisfied.
pub const WAITABLE_STATES: [&str; 3] = ["blocked", "idle", "working"];

/// The four outcomes the coordinator's wait answers with.
const WAIT_OUTCOMES: [&str; 4] = [
    "matched",
    "timed_out",
    "occupant_changed",
    "session_closed",
];

/// Every observed agent status, sorted, as a table or as one JSON array.
pub async fn list(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let response = api
        .answer(api.stub().agent_status_list(AgentStatusListRequest::default()))
        .await?;
    let mut views = response.statuses;
    views.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    let projected = views
        .iter()
        .map(agent_projection::project)
        .collect::<Result<Vec<_>, _>>()?;
    if args.has("--json") {
        out.answer(&agent_projection::json_line(&projected)?);
        return Ok(ExitCode::SUCCESS);
    }
    out.answer(TSV_HEADER);
    for status in &projected {
        out.answer(&status.tsv_row());
    }
    Ok(ExitCode::SUCCESS)
}

/// One session's observed agent status, as one table row or one JSON document.
pub async fn status(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let view = read(api, session).await?;
    let projected = agent_projection::project(&view)?;
    if args.has("--json") {
        out.answer(&agent_projection::json_line(&projected)?);
        return Ok(ExitCode::SUCCESS);
    }
    out.answer(TSV_HEADER);
    out.answer(&projected.tsv_row());
    Ok(ExitCode::SUCCESS)
}

/// Park until the session's agent reaches one of the named states, or the
/// window closes. Exit 0 only when it did; the state is on stdout either way,
/// because a caller that timed out still asked what state the agent is in.
pub async fn wait(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let states = parse_states(args.value("--until")?, "agent-wait")?;
    let timeout_ms = parse_duration(args.value("--timeout")?, "agent-wait")?;

    let pinned_view = read(api, session).await?;
    let fence = agent_projection::fields(&pinned_view)?;
    let identity = agent_status_identity(&fence).ok_or_else(|| {
        CommandFailure::usage(format!(
            "agent-wait: the coordinator's status for {session} carries no occupant identity, so \
             there is nothing to park on. A worker predating durable observation reports state it \
             cannot fence."
        ))
    })?;

    let outcome = api
        .answer(api.stub().agent_status_wait(AgentStatusWaitRequest {
            session_id: session.to_string(),
            status_epoch: identity.status_epoch.as_str().to_string(),
            occupant_id: identity.occupant_id.as_str().to_string(),
            desired_states: states,
            timeout_ms,
            ..Default::default()
        }))
        .await?
        .outcome;
    if !WAIT_OUTCOMES.contains(&outcome.as_str()) {
        return Err(CommandFailure::generic(format!(
            "agent-wait: the coordinator answered the wait with {outcome:?}, which is not one of {}",
            WAIT_OUTCOMES.join(", ")
        )));
    }
    out.progress(&format!("agent-wait: outcome {outcome}"));

    let observed = observe(api, session, &pinned_view, &fence, out).await?;
    out.answer(&observed.state);
    Ok(if outcome == "matched" {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(GENERIC_FAILURE)
    })
}

/// One status read, or the refusal that says the coordinator returned none.
///
/// "None" is not an empty answer to be printed as a blank: the field is a
/// message, and a message that is absent means the coordinator holds no
/// observation, which is a different fact from a session with no agents.
async fn read(api: &CoordinatorApi, session: &str) -> Result<AgentStatusView, CommandFailure> {
    let mut response = api
        .answer(api.stub().agent_status_get(AgentStatusGetRequest {
            session_id: session.to_string(),
            ..Default::default()
        }))
        .await?;
    response.status.take().ok_or_else(|| {
        CommandFailure::generic(format!(
            "the coordinator at {} returned no agent status for {session}",
            api.origin()
        ))
    })
}

/// The state the session's agent was in once the wait returned.
struct Observed {
    state: String,
}

async fn observe(
    api: &CoordinatorApi,
    session: &str,
    pinned_view: &AgentStatusView,
    pinned: &AgentStatusFields,
    out: &mut dyn ApiOutput,
) -> Result<Observed, CommandFailure> {
    let view = match read(api, session).await {
        Ok(view) => view,
        Err(missing) => {
            // The occupant was released while the wait was parked. The fence
            // the caller holds is still the honest answer to "which agent", and
            // the state that goes with it is the last state it reported.
            out.progress(&format!("agent-wait: {missing}"));
            return Ok(Observed {
                state: pinned.state.as_str().to_string(),
            });
        }
    };
    let after = agent_projection::fields(&view)?;
    let held = AgentStatus {
        common: pinned.clone(),
        active: pinned_view.active,
    };
    let update = AgentStatusUpdate {
        common: after.clone(),
        active: view.active,
    };
    let same_occupant = same_agent_status_occupant(&held.common, &update.common);
    // Seeded from the row the wait was parked on, so the re-read is judged as
    // an update over that row rather than as a first observation.
    let order = AgentStatusOrder::seeded_from(Some(&held));
    if !same_occupant {
        out.progress(&format!(
            "agent-wait: the occupant of {session} was replaced while the wait was parked; \
             reporting the state the wait was pinned to"
        ));
        return Ok(Observed {
            state: pinned.state.as_str().to_string(),
        });
    }
    if !order.accepts(Some(&held), &update) {
        return Ok(Observed {
            state: pinned.state.as_str().to_string(),
        });
    }
    Ok(Observed {
        state: after.state.as_str().to_string(),
    })
}

/// `--until`'s comma list, as the closed vocabulary the protocol defines.
pub fn parse_states(value: &str, verb: &str) -> Result<Vec<String>, CommandFailure> {
    let invalid = || {
        CommandFailure::usage(format!(
            "{verb}: --until must be a unique comma-list of {}",
            WAITABLE_STATES.join(",")
        ))
    };
    let mut states: Vec<String> = Vec::new();
    for state in value.split(',') {
        if !WAITABLE_STATES.contains(&state) || states.iter().any(|seen| seen == state) {
            return Err(invalid());
        }
        states.push(state.to_string());
    }
    if states.is_empty() {
        return Err(invalid());
    }
    Ok(states)
}

/// `--timeout`'s integer duration, bounded by the protocol's own limits.
pub fn parse_duration(value: &str, verb: &str) -> Result<u32, CommandFailure> {
    let invalid = || {
        CommandFailure::usage(format!(
            "{verb}: --timeout must be an integer duration from {AGENT_PROMPT_WAIT_TIMEOUT_MIN_MS}ms \
             to {}m (for example 30s)",
            AGENT_PROMPT_WAIT_TIMEOUT_MAX_MS / 60_000
        ))
    };
    let (digits, multiplier) = if let Some(millis) = value.strip_suffix("ms") {
        (millis, 1_i64)
    } else if let Some(seconds) = value.strip_suffix('s') {
        (seconds, 1_000)
    } else if let Some(minutes) = value.strip_suffix('m') {
        (minutes, 60_000)
    } else {
        return Err(invalid());
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    let amount: i64 = digits.parse().map_err(|_| invalid())?;
    let millis = amount.checked_mul(multiplier).ok_or_else(invalid)?;
    validate_agent_prompt_wait_timeout_ms("--timeout", millis).map_err(|_| invalid())?;
    u32::try_from(millis).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::{parse_duration, parse_states};
    use crate::command_error::REJECTED_INVOCATION;

    #[test]
    fn a_wait_names_states_the_protocol_defines_and_nothing_else() {
        assert_eq!(
            parse_states("working,idle", "agent-wait"),
            Ok(vec!["working".to_string(), "idle".to_string()])
        );
        for bad in ["", "thinking", "working,working", "working, "] {
            let failure = parse_states(bad, "agent-wait")
                .expect_err("an unknown or repeated state cannot be waited for");
            assert_eq!(failure.code, REJECTED_INVOCATION, "{bad:?} was accepted");
        }
    }

    #[test]
    fn a_timeout_is_read_at_the_protocols_own_bounds() {
        assert_eq!(parse_duration("30s", "agent-wait"), Ok(30_000));
        assert_eq!(parse_duration("500ms", "agent-wait"), Ok(500));
        assert_eq!(parse_duration("5m", "agent-wait"), Ok(300_000));
        for bad in ["0s", "301s", "5m1s", "30", "30 s", "-1s", "1h"] {
            let failure = parse_duration(bad, "agent-wait")
                .expect_err("a window outside the protocol's bounds is not a window");
            assert_eq!(failure.code, REJECTED_INVOCATION, "{bad:?} was accepted");
        }
    }
}
