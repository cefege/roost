//! The `roost-worker` entry point: arguments, the process environment, and the
//! exit code. Every decision about what the worker does lives in
//! `roost_worker::runtime`.
//!
//! This file exists so the worker can be run on its own, which is what a
//! developer does and what an installed layout does not. `roost worker` is the
//! product's spelling of the same call, and both go through
//! `roost_worker::runtime::serve` — one implementation and one boot order, so
//! there is no second sequence to keep in step.
//!
//! It is exempt from the no-stdout rule because `--help`, `--version` and a
//! usage error are this program's interface to whoever typed them. Routing those
//! through a log subscriber would make them invisible to the person who asked.
//! Everything else the worker says goes through `tracing`.

#![forbid(unsafe_code)]

use std::process::ExitCode;

use roost_host::{ProcessEnv, supported_host_platform};
use roost_worker::runtime::boot::WorkerOverrides;
use roost_worker::runtime::{WorkerBoot, serve};

const USAGE: &str = "\
roost-worker — the Roost worker service

USAGE:
    roost-worker [OPTIONS]

OPTIONS:
    --coordinator <URL>     The coordinator to dial. Overrides ROOST_COORDINATOR_URL.
    --keeper-socket <PATH>  The keeper socket to adopt or start. Overrides
                            ROOST_KEEPER_SOCKET.
    --keeper-executable <PATH>
                            The roost-keeper to start when there is nothing to
                            adopt. Defaults to the roost-keeper beside this binary.
    -h, --help              Print this message.
    -V, --version           Print the version.

IDENTITY
    There is no identity option, and there is no identity variable. The
    fingerprint this worker dials and registers as is the SHA-256 of the public
    key in <data>/coordinator_ed25519.key, and the coordinator credential is
    signed from that same key, so the dial path and the token cannot disagree
    and neither can be set to a value the coordinator has never seen. A machine
    with no key is given one at mode 0600 on first boot; ROOST_WORKER_KEY_PATH
    moves it.

    Every other option has an ROOST_* environment variable, and the environment
    is what an installed service definition supplies. The remaining paths
    default to the worker data directory: <data>/mux-keeper.sock and
    <data>/mux-keeper.pid.

    The worker holds no PTYs of its own. The keeper does, and it is meant to
    outlive this process, so a coordinator outage or a worker restart costs a
    reconnect and not a terminal. Only a signal, or an explicit shutdown frame
    from the coordinator, ends this process.
";

/// The value after a named option, and the step past both arguments.
fn take_value(argv: &[String], index: &mut usize, name: &str) -> Result<String, String> {
    let value = argv
        .get(*index + 1)
        .cloned()
        .ok_or_else(|| format!("{name} needs a value"))?;
    *index += 2;
    Ok(value)
}

/// Parse the command line.
///
/// `--help` and `--version` print their own answer and return `Ok(None)`: both
/// are the program's interface, and neither is a boot anybody asked for. The
/// caller exits rather than continuing into one.
fn parse_args(argv: &[String]) -> Result<Option<WorkerOverrides>, String> {
    let mut overrides = WorkerOverrides::default();
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "-h" | "--help" => {
                // A `--help` that prints nothing is a broken `--help`.
                println!("{USAGE}");
                return Ok(None);
            }
            "-V" | "--version" => {
                println!("roost-worker {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--coordinator" => {
                overrides.coordinator = Some(take_value(argv, &mut index, "--coordinator")?);
            }
            "--keeper-socket" => {
                overrides.keeper_socket = Some(take_value(argv, &mut index, "--keeper-socket")?);
            }
            "--keeper-executable" => {
                overrides.keeper_executable =
                    Some(take_value(argv, &mut index, "--keeper-executable")?);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(Some(overrides))
}

/// A configuration with the command line's values laid over the environment's.
///
/// The environment is read and then discarded, never rewritten: a process that
/// mutates its own environment is a process whose configuration depends on the
/// order two libraries happened to read it in.
fn resolve_boot(overrides: WorkerOverrides) -> Result<WorkerBoot, String> {
    let platform = supported_host_platform().map_err(|error| error.to_string())?;
    let mut boot =
        WorkerBoot::resolve(&ProcessEnv::new(), platform).map_err(|error| error.to_string())?;
    boot.apply(overrides).map_err(|error| error.to_string())?;
    Ok(boot)
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let overrides = match parse_args(&argv) {
        // `--help` and `--version` have already printed their answer.
        Ok(None) => return ExitCode::SUCCESS,
        Ok(Some(overrides)) => overrides,
        Err(reason) => {
            eprintln!("roost-worker: {reason}\n\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let boot = match resolve_boot(overrides) {
        Ok(boot) => boot,
        Err(reason) => {
            // Refused before anything is started: no socket is bound, no keeper
            // is probed, no frame is written. That is the whole reason resolution
            // is a separate step from serving.
            eprintln!("roost-worker: {reason}");
            return ExitCode::FAILURE;
        }
    };
    match serve(boot) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // The runtime is gone, so a `tracing` event here would reach nobody
            // on a default subscriber. One line on stderr is the interface.
            eprintln!("roost-worker: {error:#}");
            ExitCode::FAILURE
        }
    }
}
