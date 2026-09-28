//! The `PATH` a worker runs its own tools with, the `PATH` a PTY is given, and
//! the one bounded way this crate runs a host tool. Read by the shell-spec
//! resolver, the samplers, the ports reader, the `gh` reader and the tailnet
//! resolver, so the package-manager directories a service manager leaves off
//! `PATH` — and the timeout that keeps a hung tool from taking a sampling
//! thread with it — are named once. Depends on `roost_host::HostPlatform` and
//! `std`; nothing here.
//!
//! TWO PREFIXES, ONE RULE. A worker's launchd/LaunchAgent or systemd unit
//! carries a minimal `PATH`; `gh`, `lsof`, `ip` and `ss` are not on it. A bare
//! spawn then ENOENTs and the feature that needed it — the PR badge — silently
//! never resolves, with no error anywhere. The fix is the same in both places,
//! and the two lists differ for one reason: `/usr/sbin` holds root's tools, and
//! a PTY is the user's shell rather than this daemon. The tool prefix is v2's
//! `listening-ports.ts` darwin `TOOL_PATH`; v2's `pr-status.ts` `GH_PATH` and
//! its linux ports path are the same directories minus one that holds neither
//! `gh` nor `ss` on any supported host.

use std::io::Read as _;
use std::path::Path;
use std::time::{Duration, Instant};

/// The directories PREPENDED to a PTY's inherited `PATH`.
///
/// Order is the package managers' own: Apple Silicon's homebrew, then Intel's,
/// then the system directories. `zsh`/`bash` hash these at first use, so a
/// directory added behind them would be found late.
pub const PTY_PATH_PREFIX: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

/// How long a host tool may run before it is killed and its reading is
/// discarded, for the tools v2 ran through `Bun.spawn` with no timeout at all
/// (`git`, `gh`, `ps`, `ss`, `lsof`). Ten seconds because the slowest caller is
/// `gh`, a network round trip.
///
/// This bound is a v3 choice and NOT a port: v2 inherited a hung `gh` into a
/// hung poll. The samplers and the tailnet resolver pass v2's own bounds to
/// [`run_bounded`] instead.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(10);

/// The `PATH` this worker resolves its own tools against, prefix included.
///
/// The inherited `PATH` is kept behind the prefix rather than replacing it: a
/// tool installed into a directory only this account has on `PATH` stays
/// reachable, and the prefix only decides what is reachable that was not.
pub fn tool_path(inherited: Option<&str>, platform: roost_host::HostPlatform) -> String {
    let prefix = match platform {
        roost_host::HostPlatform::MacOs | roost_host::HostPlatform::Linux => {
            "/opt/homebrew/bin:/usr/local/bin:/usr/sbin:/usr/bin:/bin"
        }
        // v3 ships Linux and macOS only. The spelling is kept so a caller that
        // is handed a Windows platform by a stored value gets a refusal from
        // the platform check rather than a PATH built for a host that is not
        // supported.
        roost_host::HostPlatform::Windows => "/usr/bin:/bin",
    };
    match inherited.map(str::trim).filter(|value| !value.is_empty()) {
        Some(rest) => format!("{prefix}:{rest}"),
        None => prefix.to_string(),
    }
}

/// The tool `PATH` built over this process's own `PATH`.
pub fn process_tool_path(platform: roost_host::HostPlatform) -> String {
    let inherited = std::env::var("PATH").ok();
    tool_path(inherited.as_deref(), platform)
}

/// Run a program, resolved on this process's `PATH`, inside [`TOOL_TIMEOUT`].
pub fn run(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    run_bounded(program, args, cwd, None, TOOL_TIMEOUT)
}

/// Run a program with `path` as its `PATH`, inside [`TOOL_TIMEOUT`].
///
/// The program is looked up on `path` too: a `PATH` set on the command is the
/// one the child's `execvp` searches, which is the whole point of passing it.
pub fn run_on_path(path: &str, program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    run_bounded(program, args, cwd, Some(path), TOOL_TIMEOUT)
}

/// Run a program and return its stdout, if it succeeded inside `timeout`.
///
/// `cwd` is the directory it runs IN: `git rev-parse` answers about the
/// repository it is standing in. Stdout is drained on its own thread while the
/// exit is polled, because a tool whose output outgrows the pipe buffer (`ps`
/// on a busy host, `ss` with many sockets) blocks on the write and would
/// otherwise read as a hang. `None` for every failure — absent, non-zero,
/// killed on the deadline, undecodable output — because a caller that could
/// not read a counter reports a zero rather than an incident.
pub fn run_bounded(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    path: Option<&str>,
    timeout: Duration,
) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(directory) = cwd {
        command.current_dir(directory);
    }
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::Builder::new()
        .name("roost-tool-stdout".to_string())
        .spawn(move || {
            let mut out = Vec::new();
            stdout.read_to_end(&mut out).map(|_| out)
        })
        .ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                let out = reader.join().ok()?.ok()?;
                return String::from_utf8(out).ok();
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                tracing::debug!(program, ?timeout, "a host tool was killed on its deadline");
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}
