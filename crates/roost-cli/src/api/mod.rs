//! `roost api <verb>` — the headless client: introspect and drive a running
//! coordinator from a terminal, with no browser. Called by the crate's
//! dispatcher; every verb reaches the wire through the generated Connect stubs
//! in `roost-proto`, and the credential is resolved by `api::credentials`.
//!
//! TWO PROPERTIES ARE LOAD-BEARING HERE. First, no verb names a method: each
//! one calls `api::client`'s generated stub, whose spec carries the service and
//! method names, so a rename in `protocol/proto/roost/v1/*.proto` breaks this
//! build instead of answering 404 at the moment an operator is scripting a
//! fleet change. Second, `agent-status --json` is the one machine-readable
//! result in the command, so every other verb keeps stdout to the answer and
//! sends progress, ranges and remedies to stderr — which is why the streams
//! arrive here as an `ApiOutput` rather than as a `println!`.
//!
//! THE DISPATCH IS A MATCH, NOT A TABLE OF CLOSURES. The table in
//! `api::verbs` says what a verb is called and what it takes; what it *does*
//! is a match arm here, so `grep` for a verb's behaviour finds exactly one
//! place and the table cannot grow a row nobody dispatches.

pub mod agent_prompt;
pub mod agent_projection;
pub mod agents;
pub mod client;
pub mod credentials;
pub mod device;
pub mod output;
pub mod scrollback;
pub mod sessions;
pub mod tasks;
pub mod ui;
pub mod verbs;
pub mod workers;
pub mod workspaces;

use std::process::ExitCode;

use clap::Args;
use roost_host::ProcessEnv;

use crate::api::client::CoordinatorApi;
use crate::api::output::{ApiOutput, TerminalOutput};
use crate::command_error::{CommandFailure, GENERIC_FAILURE};

/// `roost api <verb> [<args>...]`.
#[derive(Debug, Args)]
#[command(
    name = "api",
    about = "Drive a running coordinator from the terminal, with no browser"
)]
pub struct ApiArgs {
    /// The verb. Omit it, or name one this build does not answer, and the list
    /// is printed with exit code 2.
    pub verb: Option<String>,
    /// The verb's own arguments.
    ///
    /// Each verb reads its own, because the shapes disagree: `input` takes text
    /// that must not be read as an option, `ws-set-sessions` takes a list that
    /// runs until the next option, and `rename` takes a title an operator types
    /// as several words. Clap cannot be given one grammar for all of them, and
    /// a verb grammar invented here would be a second answer to "what does
    /// this verb take" beside the table in `api::verbs`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

/// Run one `roost api` invocation, writing to the process's own streams.
pub async fn run(args: &ApiArgs) -> Result<ExitCode, CommandFailure> {
    let mut output = TerminalOutput;
    run_with(args.verb.as_deref(), &args.args, &mut output).await
}

/// The same command, over an injectable pair of streams.
///
/// Public because it is the seam the behaviour tests drive: they read what a
/// verb wrote, so the promise that `agent-status --json` is alone on stdout is
/// a thing a test can check rather than a thing the source says.
pub async fn run_with(
    verb: Option<&str>,
    args: &[String],
    output: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    dispatch(verb, args, output).await
}

async fn dispatch(
    verb: Option<&str>,
    args: &[String],
    output: &mut dyn ApiOutput,
) -> Result<ExitCode, CommandFailure> {
    let Some(verb) = verb else {
        return Err(verbs::missing_verb());
    };
    let Some(spec) = verbs::lookup(verb) else {
        return Err(verbs::unknown_verb(verb));
    };
    let parsed = verbs::parse(spec, args)?;
    match spec.verb {
        // Two verbs v2 registered only to refuse. They answer, because an
        // answer naming the replacement is more use to a stale script than a
        // usage error about a name that used to work.
        "cat" | "watch" => {
            output.progress(spec.usage);
            Ok(ExitCode::from(GENERIC_FAILURE))
        }
        "agents" => agents::list(&link()?, &parsed, output).await,
        "agent-status" => agents::status(&link()?, &parsed, output).await,
        "agent-wait" => agents::wait(&link()?, &parsed, output).await,
        "agent-prompt" => agent_prompt::prompt(&link()?, &parsed, output).await,
        "sessions" => sessions::list(&link()?, &parsed, output).await,
        "input" => sessions::input(&link()?, &parsed, output).await,
        "rename" => sessions::rename(&link()?, &parsed, output).await,
        "assign" => sessions::assign(&link()?, &parsed, output).await,
        "spawn" => sessions::spawn(&link()?, &parsed, output).await,
        "kill" => sessions::kill(&link()?, &parsed, output).await,
        "cells" => scrollback::cells(&link()?, &parsed, output).await,
        "attach" => scrollback::attach(&link()?, &parsed, output).await,
        "workers" => workers::list(&link()?, &parsed, output).await,
        "worker-rename" => workers::rename(&link()?, &parsed, output).await,
        "worker-rm" => workers::remove(&link()?, &parsed, output).await,
        "workspaces" => workspaces::list(&link()?, &parsed, output).await,
        "ws-create" => workspaces::create(&link()?, &parsed, output).await,
        "ws-update" => workspaces::update(&link()?, &parsed, output).await,
        "ws-delete" => workspaces::delete(&link()?, &parsed, output).await,
        "ws-set-sessions" => workspaces::set_sessions(&link()?, &parsed, output).await,
        "tasks" => tasks::list(&link()?, &parsed, output).await,
        "task-enqueue" => tasks::enqueue(&link()?, &parsed, output).await,
        "task-cancel" => tasks::cancel(&link()?, &parsed, output).await,
        "ui" => ui::command(&link()?, &parsed, output).await,
        "ui-state" => ui::state(&link()?, &parsed, output).await,
        "device-revoke-local" => {
            let platform = roost_host::supported_host_platform()?;
            device::revoke_local(&ProcessEnv::new(), platform, &parsed, output).await
        }
        other => Err(verbs::unknown_verb(other)),
    }
}

/// The coordinator link a verb talks through, resolved from this process's
/// environment and from the machine's installed coordinator definition.
fn link() -> Result<CoordinatorApi, CommandFailure> {
    let platform = roost_host::supported_host_platform()?;
    CoordinatorApi::from_environment(&ProcessEnv::new(), platform)
}
