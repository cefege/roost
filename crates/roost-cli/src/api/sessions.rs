//! `roost api sessions`, `input`, `spawn`, `kill`, `rename` and `assign`: the
//! verbs that list the sessions a coordinator holds and act on one of them.
//! Called by `api::mod` after it has decided the verb and parsed the
//! arguments; depends on the generated `Sessions*` methods, on
//! `roost-protocol`'s prompt byte cap, and on `api::client`.
//!
//! WHAT GOES TO STDOUT. A session id, a boolean the coordinator returned, and
//! the one JSON document `sessions --json` publishes. Nothing else: the range a
//! read covers, the bytes an upload is at, and every refusal are this
//! command's own business, and `sessions --json` exists to be piped into `jq`,
//! which stops at the first line that is not the document.

use std::io::Read;
use std::process::ExitCode;

use roost_proto::{
    SessionsAssignWorkspaceRequest, SessionsInputRequest, SessionsKillRequest, SessionsListRequest,
    SessionsRenameRequest, SessionsSpawnRequest,
};
use serde::Serialize;

use crate::api::client::CoordinatorApi;
use crate::api::output::ApiOutput;
use crate::api::verbs::Invocation;
use crate::command_error::CommandFailure;

/// The largest `--stdin` payload `input` will send, and with `--enter` the
/// largest that leaves room for the carriage return.
///
/// 64 KiB, not `roost-protocol`'s agent-prompt cap: this is raw keystrokes
/// into an arbitrary shell, where the binding limit is what a PTY will take in
/// one write rather than what an agent will accept as a prompt.
pub const MAX_INPUT_BYTES: usize = 65_536;

/// The carriage return a PTY reads as Enter.
const ENTER: u8 = b'\r';

/// Every session the coordinator holds, open or closed, sorted by id.
pub async fn list(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let response = api
        .answer(api.stub().sessions_list(SessionsListRequest {
            status: Some("all".to_string()),
            ..Default::default()
        }))
        .await?;
    let mut sessions = response.sessions;
    sessions.sort_by(|left, right| left.id.cmp(&right.id));
    if args.has("--json") {
        let published: Vec<SessionProjection> =
            sessions.iter().map(SessionProjection::from_proto).collect();
        let encoded = serde_json::to_string_pretty(&published)
            .map_err(|error| CommandFailure::generic(format!("sessions: {error}")))?;
        out.answer(&encoded);
        return Ok(ExitCode::SUCCESS);
    }
    out.answer("id\tworker\tkind\tcwd\ttitle");
    for session in &sessions {
        let published = SessionProjection::from_proto(session);
        out.answer(&format!(
            "{}\t{}\t{}\t{}\t{}",
            published.id, published.worker_fp, published.kind, published.cwd, published.title
        ));
    }
    Ok(ExitCode::SUCCESS)
}

/// One session, in the shape `sessions --json` has always published.
#[derive(Debug, Serialize)]
struct SessionProjection {
    id: String,
    #[serde(rename = "workerFp")]
    worker_fp: String,
    cwd: String,
    #[serde(rename = "spawnCwd")]
    spawn_cwd: String,
    title: String,
    /// The spawn kind (`shell`), which is NOT the session's status. v2's table
    /// published `session.kind` in this column, and a reader who greps the
    /// wire for `kind` must not find a status wearing its name.
    kind: String,
    status: String,
}

impl SessionProjection {
    fn from_proto(session: &roost_proto::Session) -> Self {
        Self {
            id: session.id.clone(),
            worker_fp: session.worker_fp.clone(),
            cwd: session.cwd.clone(),
            spawn_cwd: String::new(),
            title: session.custom_title.clone().unwrap_or_default(),
            kind: session.kind.clone(),
            status: session.status.clone(),
        }
    }
}

/// Send exact bytes into a session's PTY.
pub async fn input(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let from_stdin = args.has("--stdin");
    let text = args.optional_positional(1);
    if from_stdin && text.is_some() {
        return Err(CommandFailure::usage(
            "roost api input: --stdin cannot be combined with text",
        ));
    }
    if !from_stdin && text.is_none() {
        return Err(CommandFailure::usage(
            "roost api input: give text or --stdin",
        ));
    }
    let mut data = match (from_stdin, text) {
        (true, _) => read_stdin()?,
        (false, Some(text)) => expand_escapes(text).into_bytes(),
        (false, None) => Vec::new(),
    };
    if args.has("--enter") {
        data.push(ENTER);
    }
    let ceiling = if args.has("--enter") {
        MAX_INPUT_BYTES - 1
    } else {
        MAX_INPUT_BYTES
    };
    if data.len() > ceiling {
        return Err(CommandFailure::usage(format!(
            "roost api input: {} bytes exceeds the {ceiling} byte limit",
            data.len()
        )));
    }
    let accepted = api
        .answer(api.stub().sessions_input(SessionsInputRequest {
            session_id: session.to_string(),
            data,
            ..Default::default()
        }))
        .await?
        .accepted;
    out.answer(if accepted {
        "{\"ok\":true,\"accepted\":true}"
    } else {
        "{\"ok\":false,\"accepted\":false,\"error\":\"terminal input was not accepted\"}"
    });
    Ok(if accepted {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(crate::command_error::REJECTED_INVOCATION)
    })
}

/// Open a shell session on a worker.
pub async fn spawn(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let worker = args.positional(0, "worker")?;
    let folder = args.positional(1, "folder")?;
    let response = api
        .answer(api.stub().sessions_spawn(SessionsSpawnRequest {
            worker_fp: worker.to_string(),
            kind: "shell".to_string(),
            folder: folder.to_string(),
            ..Default::default()
        }))
        .await?;
    out.answer(&format!(
        "{{\"sessionId\":{},\"channelId\":{}}}",
        json_string(&response.session_id),
        response.channel_id
    ));
    Ok(ExitCode::SUCCESS)
}

/// Tombstone a session. `--force` is not offered: an operator who means to end
/// a session holding live PTYs has a session id and can say so, and a flag on a
/// destructive verb is one more thing to type by accident.
pub async fn kill(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let accepted = api
        .answer(api.stub().sessions_kill(SessionsKillRequest {
            session_id: session.to_string(),
            force: false,
            ..Default::default()
        }))
        .await?
        .accepted;
    out.answer(&accepted.to_string());
    Ok(ExitCode::SUCCESS)
}

/// Set or clear a session's operator-facing title. An empty title clears the
/// override and the session goes back to its auto title.
pub async fn rename(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let title = args.joined_from(1);
    let ok = api
        .answer(api.stub().sessions_rename(SessionsRenameRequest {
            session_id: session.to_string(),
            title,
            ..Default::default()
        }))
        .await?
        .ok;
    out.answer(&ok.to_string());
    Ok(ExitCode::SUCCESS)
}

/// Put a session in a workspace. The literal `--` clears the assignment, which
/// is why it is a word and not an empty argument.
pub async fn assign(
    api: &CoordinatorApi,
    args: &Invocation,
    out: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let session = args.positional(0, "session")?;
    let workspace = args.positional(1, "workspace-id|--")?;
    // The literal `--` is how an operator says "no workspace": the request's
    // field is optional, and an empty string would be a workspace named "".
    let workspace_id = (workspace != "--").then(|| workspace.to_string());
    let ok = api
        .answer(
            api.stub()
                .sessions_assign_workspace(SessionsAssignWorkspaceRequest {
                    session_id: session.to_string(),
                    workspace_id,
                    ..Default::default()
                }),
        )
        .await?
        .ok;
    out.answer(&ok.to_string());
    Ok(ExitCode::SUCCESS)
}

/// A quoted JSON string, so a session id with a quote in it cannot break the
/// one document this verb prints.
fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn read_stdin() -> Result<Vec<u8>, CommandFailure> {
    let mut buffer = Vec::new();
    std::io::stdin()
        .read_to_end(&mut buffer)
        .map_err(|error| CommandFailure::generic(format!("roost api input: stdin: {error}")))?;
    Ok(buffer)
}

/// `\n`, `\t` and `\r` in a shell argument, which a shell eats before this
/// process ever sees them. Everything else is left exactly as typed.
fn expand_escapes(text: &str) -> String {
    let mut expanded = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            expanded.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => expanded.push('\n'),
            Some('t') => expanded.push('\t'),
            Some('r') => expanded.push('\r'),
            Some('\\') => expanded.push('\\'),
            Some(other) => {
                expanded.push('\\');
                expanded.push(other);
            }
            None => expanded.push('\\'),
        }
    }
    expanded
}

#[cfg(test)]
mod tests {
    use super::expand_escapes;

    #[test]
    fn an_escape_a_shell_would_have_eaten_reaches_the_terminal() {
        assert_eq!(expand_escapes("a\\nb"), "a\nb");
        assert_eq!(expand_escapes("a\\tb"), "a\tb");
        assert_eq!(expand_escapes("a\\rb"), "a\rb");
    }

    #[test]
    fn an_unrecognised_escape_stays_two_characters_rather_than_being_swallowed() {
        assert_eq!(expand_escapes("a\\qb"), "a\\qb");
        assert_eq!(expand_escapes("trailing\\"), "trailing\\");
    }
}
