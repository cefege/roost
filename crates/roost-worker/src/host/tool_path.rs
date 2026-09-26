//! The `PATH` a worker runs its own tools with, the `PATH` a PTY is given, and
//! the one bounded way this crate runs a host tool. Read by the shell-spec
//! resolver, the samplers, the ports reader and the `gh` reader, so the
//! package-manager directories a service manager leaves off `PATH` — and the
//! timeout that keeps a hung tool from taking a sampling thread with it — are
//! named once. Depends on `roost_host::HostPlatform` and `std`; nothing here.
//!
//! TWO PREFIXES, ONE RULE. A worker's launchd/LaunchAgent or systemd unit
//! carries a minimal `PATH`; `gh`, `lsof`, `ip` and `ss` are not on it. A bare
//! spawn then ENOENTs and the feature that needed it — the PR badge — silently
//! never resolves, with no error anywhere. The fix is the same in both places,
//! and the two lists differ for one reason: `/usr/sbin` holds root's tools, and
//! a PTY is the user's shell rather than this daemon.

use std::path::Path;

/// The directories PREPENDED to a PTY's inherited `PATH`.
///
/// Order is the package managers' own: Apple Silicon's homebrew, then Intel's,
/// then the system directories. `zsh`/`bash` hash these at first use, so a
/// directory added behind them would be found late.
pub const PTY_PATH_PREFIX: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin";

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

/// Run a program and return its stdout, if it succeeded inside the timeout.
///
/// `cwd` is the directory it runs IN, which several of these tools require:
/// `git rev-parse` answers about the repository it is standing in, not about
/// one named on the command line. Blocking with a deadline rather than spawning
/// a task: the callers are sampling threads on the worker's own runtime, and a
/// tool that hangs must not hold one. `None` for every failure — absent,
/// non-zero, killed on the deadline, undecodable output — because a caller that
/// could not read a counter reports a zero rather than an incident.
pub fn run(program: &str, args: &[&str], cwd: Option<&Path>) -> Option<String> {
    use std::process::{Command, Stdio};
    use std::time::Instant;
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(directory) = cwd {
        command.current_dir(directory);
    }
    let mut child = command.spawn().ok()?;
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => {
                return child
                    .wait_with_output()
                    .ok()
                    .and_then(|out| String::from_utf8(out.stdout).ok());
            }
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    }
}
