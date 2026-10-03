//! The keeper daemon: bind the endpoint, serve a worker, and decide when to
//! stop. Everything protocol-shaped is in the library; this binary is argument
//! parsing, signals, and the exit decision.
//!
//! The exit decision is the part worth reading. A worker that merely
//! disconnected must NOT take the keeper down with it — the whole point of the
//! keeper is that it outlives the worker, so a worker restart costs a reconnect
//! and not a terminal. An explicit shutdown stops the process (a conditional
//! one only when the keeper is actually empty), and so do the two things no
//! worker sends: a termination signal, and the socket file being deleted.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use roost_keeper::server::{
    Accepted, ConnectionEnd, Endpoint, ExitCause, ExitWatch, SOCKET_CHECK_INTERVAL, Server,
};

/// Set by the signal handler, which may only touch async-signal-safe state.
/// The server's exit watch reads it on every connection turn and at least every
/// quarter second while waiting for a worker, so a signal is never parked
/// behind a blocked `accept`.
static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_terminate(_signal: std::ffi::c_int) {
    STOP_REQUESTED.store(true, Ordering::SeqCst);
}

const USAGE: &str = "\
roost-keeper — the PTY-owning daemon

USAGE:
    roost-keeper [OPTIONS]

OPTIONS:
    --socket <PATH>   The Unix socket to listen on. Required.
    --pid-file <PATH> Where to write this process's pid, mode 0600. Optional.
    -h, --help        Print this message.
    -V, --version     Print the version.

The keeper owns every PTY it opens and outlives the worker that spawned it, so
a worker restart or a coordinator deploy costs a reconnect and not a terminal.
It stops when a worker sends Shutdown, when a worker sends ShutdownIfEmpty and
no channel is live, when it is signalled, or when its socket file is deleted.
";

/// The parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Args {
    socket: PathBuf,
    pid_file: Option<PathBuf>,
}

fn parse_args(argv: &[String]) -> Result<Option<Args>, String> {
    let mut socket = None;
    let mut pid_file = None;
    let mut index = 0;
    while index < argv.len() {
        match argv[index].as_str() {
            "-h" | "--help" => return Ok(None),
            "-V" | "--version" => {
                println!("roost-keeper {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            "--socket" => {
                let value = argv.get(index + 1).ok_or("--socket needs a path")?;
                socket = Some(PathBuf::from(value));
                index += 2;
            }
            "--pid-file" => {
                let value = argv.get(index + 1).ok_or("--pid-file needs a path")?;
                pid_file = Some(PathBuf::from(value));
                index += 2;
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let socket = socket.ok_or("--socket is required")?;
    Ok(Some(Args { socket, pid_file }))
}

/// Write this process's pid, owner-readable only.
///
/// A world-readable pid file tells any local process where to send a signal,
/// so the mode is set at creation rather than tightened afterwards.
fn write_pid_file(path: &std::path::Path) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{}", std::process::id())
}

/// Remove the pid file, but only if it is still ours.
///
/// Another keeper may have taken the path over while this one was shutting
/// down, and deleting its pid file would leave a live daemon unkillable by
/// name.
fn remove_own_pid_file(path: &std::path::Path) {
    let Ok(recorded) = std::fs::read_to_string(path) else {
        return;
    };
    if recorded.trim() == std::process::id().to_string() {
        let _ = std::fs::remove_file(path);
    }
}

/// What the daemon should do after a connection ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// Keep serving: the worker is coming back and the PTYs are its own.
    KeepServing,
    /// Stop, for the named reason.
    Stop(&'static str),
}

/// The exit decision, as a function so it is testable without a process.
fn decide(end: ConnectionEnd, live_channels: usize) -> Decision {
    // `live_channels` is reported for diagnostics even though only a signal or
    // a deleted socket stops an otherwise-idle keeper; a keeper with nothing
    // live and no worker is still worth keeping, because the worker that owned
    // those PTYs is coming back.
    let _ = live_channels;
    match end {
        ConnectionEnd::ShutdownRequested => Decision::Stop("shutdown requested"),
        ConnectionEnd::ShutdownRequestedWithChannels => {
            Decision::Stop("shutdown requested with channels live")
        }
        ConnectionEnd::ShutdownIfEmptyAccepted => Decision::Stop("shutdown if empty, and empty"),
        ConnectionEnd::ShutdownIfEmptyRefused => {
            // A channel appeared between the request and the answer. The keeper
            // stays up, and the worker is told to come back.
            Decision::KeepServing
        }
        // A disconnect is NEVER a shutdown, live channels or not. If it were,
        // a worker restart would kill every terminal on the machine, which is
        // the one outcome the keeper exists to prevent.
        ConnectionEnd::ClientDisconnected => Decision::KeepServing,
        ConnectionEnd::ProtocolViolation => Decision::KeepServing,
        ConnectionEnd::UnframeablePayload => Decision::KeepServing,
        ConnectionEnd::WorkerUnreachable => Decision::KeepServing,
        ConnectionEnd::Signalled => Decision::Stop(ExitCause::Signalled.reason()),
        ConnectionEnd::SocketRemoved => Decision::Stop(ExitCause::SocketRemoved.reason()),
    }
}

fn main() -> std::process::ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let args = match parse_args(&argv) {
        Ok(None) => {
            // A `--help` that prints nothing is a broken `--help`.
            println!("{USAGE}");
            return std::process::ExitCode::SUCCESS;
        }
        Ok(Some(args)) => args,
        Err(reason) => {
            eprintln!("roost-keeper: {reason}\n\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };

    // SAFETY: the handler only stores into a `static AtomicBool`, which is
    // async-signal-safe. It touches no allocation, no lock and no I/O.
    unsafe {
        // A function item cannot be cast to an integer, so the handler is
        // taken by pointer first; the cast then goes through a usize, which
        // is the only form this lint accepts.
        let handler = on_terminate as extern "C" fn(std::ffi::c_int) as usize;
        libc::signal(libc::SIGTERM, handler as libc::sighandler_t);
        libc::signal(libc::SIGINT, handler as libc::sighandler_t);
    }

    let endpoint = match Endpoint::new(args.socket.clone()) {
        Ok(endpoint) => endpoint,
        Err(err) => {
            eprintln!("roost-keeper: {err}");
            return std::process::ExitCode::FAILURE;
        }
    };

    // The pid file is written BEFORE the socket is published. The other order
    // leaves a window in which a worker can connect to a keeper whose pid it
    // cannot yet find, which is exactly the moment tooling needs it.
    if let Some(pid_file) = &args.pid_file
        && let Err(err) = write_pid_file(pid_file)
    {
        // Not fatal: the pid file is a convenience for tooling, and refusing to
        // own PTYs over it would be worse than running without one.
        eprintln!("roost-keeper: could not write the pid file {pid_file:?}: {err}");
    }

    let mut server = match Server::bind(endpoint) {
        Ok(server) => server,
        Err(err) => {
            eprintln!("roost-keeper: {err}");
            if let Some(pid_file) = &args.pid_file {
                remove_own_pid_file(pid_file);
            }
            return std::process::ExitCode::FAILURE;
        }
    };

    tracing::info!(socket = ?server.endpoint().path(), "keeper listening");
    let socket = server.endpoint().path().to_path_buf();
    server.watch_exits(ExitWatch::new(
        &STOP_REQUESTED,
        socket,
        SOCKET_CHECK_INTERVAL,
    ));
    let reason = loop {
        let stream = match server.accept_or_exit() {
            Accepted::Connection(stream) => stream,
            Accepted::Exit(cause) => break cause.reason(),
        };
        let end = server.serve_one(stream);
        let live = server.keeper().channel_count();
        match decide(end, live) {
            Decision::KeepServing => continue,
            Decision::Stop(reason) => break reason,
        }
    };

    // v2 `shutdown()` reaps every channel's tree before the process exits, for
    // every stop reason: a stopped keeper must not leave PTY children behind.
    server.keeper_mut().reap_all_channels();
    if let Some(pid_file) = &args.pid_file {
        remove_own_pid_file(pid_file);
    }
    tracing::info!(reason, "keeper stopping");
    // Every channel's tree was reaped above; dropping the channels here only
    // closes the master ends.
    std::process::ExitCode::SUCCESS
}
