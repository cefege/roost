//! The `roost` binary's entry point: parse, install the right logging for the
//! mode, run one command, and translate a failure into the exit code it
//! carries. Everything else lives in the library beside this file, so the
//! command tree can be exercised from a test without a process.
//!
//! Two decisions are here because they are about the PROCESS, not about any
//! command: which logging a mode gets, and the shape of the one line a failure
//! prints.

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use roost_cli::{Cli, Command, command_error::CommandFailure, dispatch};
use roost_observability::{InitOptions, LogLevel, SystemClock, init, init_with};

fn main() -> ExitCode {
    // `parse_from` writes the usage error to stderr and exits with clap's own
    // code on a bad invocation, so there is nothing left to add to it — and
    // nothing left for this function to decide either.
    let cli = Cli::parse_from(normalised_argv());
    // Captured before `dispatch` consumes the command, so the failure line
    // names what the operator actually typed.
    let command_name = cli.command.name();
    install_logging(&cli);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => return fail(&CommandFailure::generic(error.to_string()), command_name),
    };
    match runtime.block_on(dispatch(cli)) {
        Ok(code) => code,
        Err(failure) => fail(&failure, command_name),
    }
}

/// `roost --version` and `roost -v` mean `roost version`, as they did in v2.
/// clap carries no version flag here on purpose: the binary's version is
/// `roost-host`'s artifact version, which is `dev` for a source checkout, and
/// clap would print the crate version instead — two answers to one question.
fn normalised_argv() -> Vec<OsString> {
    let mut argv: Vec<OsString> = std::env::args_os().collect();
    if let Some(first) = argv
        .get(1)
        .map(|argument| argument.to_string_lossy().into_owned())
        && (first == "--version" || first == "-v")
    {
        argv[1] = OsString::from("version");
    }
    argv
}

/// A server mode's stdout and stderr are the service's own channels — the
/// service managers point them at `main.out.log` and `main.err.log`, and
/// `roost doctor` reads the second. An operator command's stdout is its
/// product, so it gets warnings only: an `info` line on stdout would land in
/// the middle of a readout some script is parsing.
fn install_logging(cli: &Cli) {
    let installed = match cli.command {
        Command::Coord(_) | Command::Worker(_) | Command::Keeper(_) => init(),
        _ => init_with(InitOptions::new(Arc::new(SystemClock)).with_min_level(LogLevel::Warn)),
    };
    // A second install can only happen if something else in this process put a
    // subscriber there first, and there is nothing else in a CLI. Either way
    // the command still runs: a missing subscriber costs a log line, not a run.
    let _ = installed;
}

/// The one machine-readable line a failure produces, on stderr. It is a single
/// JSON object with the command and the message, which is the shape v2 emitted
/// and the shape a wrapper script parses; stdout stays clean so a partially
/// completed command's output is never mistaken for a successful one.
fn fail(failure: &CommandFailure, command: &str) -> ExitCode {
    let line = serde_json::json!({ "cmd": command, "error": failure.message });
    eprintln!("{line}");
    ExitCode::from(failure.code)
}
