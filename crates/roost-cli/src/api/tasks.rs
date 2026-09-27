//! `roost api tasks`, `task-enqueue` and `task-cancel`: the verbs that read and
//! drive the coordinator's task queue. Called by `api::mod`; depends on the
//! generated `Tasks*` methods and on `api::client`.
//!
//! WHY THE PAYLOAD IS PARSED HERE AND NOT AT THE COORDINATOR. `task-enqueue`
//! takes JSON, and a payload that is not JSON would otherwise come back as an
//! opaque queue row with a coordinator-side parse error in it — a row an
//! operator then has to read to discover that their own shell quoting was
//! wrong. Parsing first makes the mistake legible at the point it was made.

use std::process::ExitCode;

use roost_proto::{TasksCancelRequest, TasksEnqueueRequest, TasksListRequest};

use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// How much of a task's payload a row shows before it is elided.
const PAYLOAD_PREVIEW_BYTES: usize = 200;

/// The queue, one row per task.
pub async fn list(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let response = api
        .answer(api.stub().tasks_list(TasksListRequest {
            state: args.optional_value("--state").map(str::to_string),
            ..Default::default()
        }))
        .await?;
    out.answer("id\tstate\tenqueued\tclaimed_by\tpayload");
    for task in &response.tasks {
        out.answer(&format!(
            "{}\t{}\t{}\t{}\t{}",
            task.id,
            task.state,
            task.enqueued_at_ms,
            task.claimed_by.as_deref().unwrap_or("-"),
            preview(&task.payload_json)
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// Put one task on the queue and print its id.
pub async fn enqueue(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let payload = args.positional(0, "payload-json")?;
    serde_json::from_str::<serde_json::Value>(payload).map_err(|error| {
        CommandFailure::usage(format!(
            "roost api task-enqueue: the payload is not JSON ({error}); the queue stores it \
             verbatim and a worker will only ever see what you enqueued"
        ))
    })?;
    let mut response = api
        .answer(api.stub().tasks_enqueue(TasksEnqueueRequest {
            payload_json: payload.to_string(),
            ..Default::default()
        }))
        .await?;
    out.answer(
        &response
            .task
            .take()
            .map_or_else(String::new, |task| task.id),
    );
    Ok(ExitCode::SUCCESS)
}

/// Cancel a queued or claimed task, printing the state it ended in.
pub async fn cancel(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let id = args.positional(0, "id")?;
    let mut response = api
        .answer(api.stub().tasks_cancel(TasksCancelRequest {
            id: id.to_string(),
            ..Default::default()
        }))
        .await?;
    out.answer(
        &response
            .task
            .take()
            .map_or_else(String::new, |task| task.state),
    );
    Ok(ExitCode::SUCCESS)
}

/// A row's payload with its runs of whitespace collapsed, so a task carrying a
/// pretty-printed document still occupies one line.
fn preview(payload: &str) -> String {
    let collapsed: Vec<&str> = payload.split_whitespace().collect();
    let joined = collapsed.join(" ");
    if joined.chars().count() <= PAYLOAD_PREVIEW_BYTES {
        return joined;
    }
    let head: String = joined
        .chars()
        .take(PAYLOAD_PREVIEW_BYTES)
        .collect::<String>();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::preview;

    #[test]
    fn a_pretty_printed_payload_still_occupies_one_row() {
        let row = preview("{\n  \"a\": 1,\n  \"b\": 2\n}");
        assert_eq!(row, "{\"a\": 1, \"b\": 2}");
    }

    #[test]
    fn a_payload_too_long_for_a_row_is_elided_rather_than_wrapped() {
        let payload = "x".repeat(500);
        let row = preview(&payload);
        assert!(row.ends_with('…'), "{row}");
        assert_eq!(row.chars().count(), 201);
    }
}
