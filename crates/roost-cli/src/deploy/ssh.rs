//! The ssh transport every remote step of a deploy goes through, and the only
//! place a remote command string is built. Called by the whole deploy group;
//! depends on `roost-platform`'s canonical POSIX quoting and on nothing else
//! in the deploy group, so a quoting change lands here once.
//!
//! Three rules make a remote deploy safe to reason about, and all three are
//! properties of this module rather than of its callers:
//!
//! 1. Every value a caller interpolates into a remote command goes through
//!    [`posix_shell_quote`]. A release path with a space or a quote in it is a
//!    release path a shell will happily rewrite into a different directory,
//!    and the failure that produces is a corrupted deploy rather than an error.
//! 2. The remote's `PATH` is set explicitly. A non-interactive ssh login runs
//!    no `.zshrc`, so without this the target's own service manager and any
//!    tool a deploy needs are simply absent.
//! 3. Nothing that reaches a remote command is read out of a file the remote
//!    sent unless it is quoted. [`reject_control_characters`] is the gate, and
//!    it runs on every value a probe reports back.

use std::path::PathBuf;
use std::process::Stdio;

use roost_host::HostPlatform;
use roost_platform::posix_shell_quote;
use tokio::process::Command;
use tracing::warn;

use crate::command_error::CommandFailure;
use crate::deploy::codes::{self, SSH_UNREACHABLE};

/// The ssh options every remote step shares.
///
/// `StrictHostKeyChecking=accept-new` auto-trusts an unknown host key on the
/// first connection and records it. Without it a target this client has never
/// seen fails with "Host key verification failed" and there is no non-interactive
/// way to answer the prompt. The keepalives bound a dead session so a stalled
/// deploy cannot hold a machine transaction open forever, and the interval is
/// deliberately generous: a worker reachable only over a relayed hop stalls past
/// any few-second tolerance, and a tighter bound tore down live transfers.
pub const SSH_OPTS: [&str; 8] = [
    "-o",
    "StrictHostKeyChecking=accept-new",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=8",
];

/// Prepended to every remote command: a search path the target can actually run
/// with. `/opt/homebrew/bin` is first because a macOS box's `launchctl` and a
/// homebrew-installed tool live there, and `$HOME/.local/bin` is where an
/// operator who installed Roost by hand put the link.
pub const REMOTE_PATH_PREFIX: &str = "export PATH=\"/opt/homebrew/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin\"; ";

/// What one remote command answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteOutcome {
    /// The remote command's exit code. `255` is ssh's own "the connection
    /// failed", which is the shape a dead remote process takes.
    pub exit: i32,
    pub stdout: String,
    pub stderr: String,
}

impl RemoteOutcome {
    /// Whether the command succeeded.
    pub fn ok(&self) -> bool {
        self.exit == 0
    }

    /// stdout and stderr as one block, for a failure message that must not lose
    /// whichever stream carried the reason.
    pub fn detail(&self) -> String {
        let stdout = if self.stdout.trim().is_empty() {
            "(empty)".to_string()
        } else {
            self.stdout.trim().to_string()
        };
        let stderr = if self.stderr.trim().is_empty() {
            "(empty)".to_string()
        } else {
            self.stderr.trim().to_string()
        };
        format!("stdout:\n{stdout}\nstderr:\n{stderr}")
    }
}

/// Refuse a value that must never reach a remote command string.
///
/// A newline, carriage return or NUL is the only thing that lets a value forge
/// a second command, and none of the three can appear in a path the target's own
/// tools produce. Refusing is therefore free, and every caller that builds a
/// command from a probed value goes through here.
pub fn reject_control_characters(what: &str, value: &str) -> Result<(), CommandFailure> {
    if value.is_empty() {
        return Err(codes::refuse(
            SSH_UNREACHABLE,
            format!("the target reported an empty {what}"),
        ));
    }
    if let Some(found) = value
        .chars()
        .find(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(codes::refuse(
            SSH_UNREACHABLE,
            format!(
                "the target reported a {what} containing {found:?}; refusing to build a remote \
                 command from it"
            ),
        ));
    }
    Ok(())
}

/// The argv for one remote command. Separate from the spawn so the shape is
/// testable without a network: `--` before the host is what makes a host that
/// looks like an option impossible to inject.
pub fn ssh_argv(host: &str, remote_command: &str, extra_opts: &[&str]) -> Vec<String> {
    let mut argv: Vec<String> = vec!["ssh".to_string()];
    argv.extend(SSH_OPTS.iter().map(|option| (*option).to_string()));
    argv.extend(extra_opts.iter().map(|option| (*option).to_string()));
    argv.push("--".to_string());
    argv.push(host.to_string());
    argv.push(format!("{REMOTE_PATH_PREFIX}{remote_command}"));
    argv
}

/// Run one remote command, capturing both streams.
pub async fn exec(host: &str, remote_command: &str) -> Result<RemoteOutcome, CommandFailure> {
    run(host, remote_command, Vec::new(), &[]).await
}

/// The same transport with `BatchMode=yes` forced on. Used for the one probe
/// whose whole purpose is to fail rather than to prompt: a deploy must never stop
/// to ask for a password on a machine it is about to replace a binary on.
pub async fn exec_batch_mode(
    host: &str,
    remote_command: &str,
) -> Result<RemoteOutcome, CommandFailure> {
    run(host, remote_command, Vec::new(), &["-o", "BatchMode=yes"]).await
}

/// Run one remote command with `input` on its standard input, which is how a
/// staged release is piped in and how an apply manifest is delivered. The payload
/// is attached to the child rather than written by hand, so a payload larger than
/// the pipe buffer cannot deadlock against a child nobody is draining yet.
pub async fn exec_with_stdin(
    host: &str,
    remote_command: &str,
    input: Vec<u8>,
) -> Result<RemoteOutcome, CommandFailure> {
    run(host, remote_command, input, &[]).await
}

async fn run(
    host: &str,
    remote_command: &str,
    input: Vec<u8>,
    extra_opts: &[&str],
) -> Result<RemoteOutcome, CommandFailure> {
    reject_control_characters("host", host)?;
    reject_control_characters("remote command", remote_command)?;
    let argv = ssh_argv(host, remote_command, extra_opts);
    let payload = StagedPayload::write(&input)?;
    let mut builder = Command::new(&argv[0]);
    builder
        .args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(file) = payload.file() {
        builder.stdin(Stdio::from(file));
    }
    let output = builder
        .output()
        .await
        .map_err(|error| codes::refuse(SSH_UNREACHABLE, format!("cannot run ssh: {error}")))?;
    drop(payload);
    Ok(RemoteOutcome {
        exit: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// A remote command's standard input, held in a file rather than in a pipe.
///
/// A pipe would need this process to write and read the same child at once, and
/// a release tarball is larger than any pipe buffer, so the write would block
/// until the child was drained. A file has neither problem and is removed as soon
/// as the child has inherited it.
struct StagedPayload {
    path: Option<PathBuf>,
}

impl StagedPayload {
    fn write(input: &[u8]) -> Result<Self, CommandFailure> {
        if input.is_empty() {
            return Ok(Self { path: None });
        }
        let path = std::env::temp_dir().join(format!(
            "roost-deploy-{}-{}.payload",
            std::process::id(),
            roost_worker::runtime::boot::new_process_epoch()
        ));
        std::fs::write(&path, input).map_err(|error| {
            codes::refuse(
                codes::REMOTE_LOST,
                format!("cannot stage the payload for {path:?}: {error}"),
            )
        })?;
        Ok(Self { path: Some(path) })
    }

    fn file(&self) -> Option<std::fs::File> {
        let path = self.path.as_ref()?;
        match std::fs::File::open(path) {
            Ok(file) => Some(file),
            Err(error) => {
                warn!(path = %path.display(), "cannot reopen a staged payload: {error}");
                None
            }
        }
    }
}

impl Drop for StagedPayload {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Prove the target answers a non-interactive login, before anything is staged
/// on it. A refusal here is exit 2 and names the thing an operator checks when
/// key-based auth is not what they expected.
pub async fn require_reachable(host: &str) -> Result<(), CommandFailure> {
    let outcome = exec_batch_mode(host, "true").await?;
    if outcome.ok() {
        return Ok(());
    }
    Err(codes::refuse(
        SSH_UNREACHABLE,
        format!(
            "ssh failed for {host}; ensure key-based auth to that host\n{}",
            outcome.detail()
        ),
    ))
}

/// The target's platform, from its own `uname`. A target that is neither
/// Darwin nor Linux is refused with exit 3 rather than being deployed to and
/// discovered broken: v3 ships no Windows broker, and this is where that fact
/// becomes a message instead of a corrupt release directory.
pub async fn remote_platform(host: &str) -> Result<HostPlatform, CommandFailure> {
    let outcome = exec(host, "uname -s").await?;
    if !outcome.ok() {
        return Err(codes::refuse(
            SSH_UNREACHABLE,
            format!(
                "cannot read the target platform from {host}\n{}",
                outcome.detail()
            ),
        ));
    }
    match outcome.stdout.trim() {
        "Linux" => Ok(HostPlatform::Linux),
        "Darwin" => Ok(HostPlatform::MacOs),
        other => Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!(
                "unsupported deploy target platform from {host}: {}; v3 deploys to Linux and \
                 macOS only",
                if other.is_empty() { "unknown" } else { other }
            ),
        )),
    }
}

/// The target's machine architecture, as `uname -m` reports it. Used to refuse a
/// release built for another machine's CPU rather than to install it and let the
/// service manager report an exec format error hours later.
pub async fn remote_arch(host: &str) -> Result<String, CommandFailure> {
    let outcome = exec(host, "uname -m").await?;
    if !outcome.ok() {
        return Err(codes::refuse(
            SSH_UNREACHABLE,
            format!(
                "cannot read the target architecture from {host}\n{}",
                outcome.detail()
            ),
        ));
    }
    let arch = outcome.stdout.trim().to_string();
    reject_control_characters("architecture", &arch)?;
    Ok(arch)
}

/// The target's home directory, resolved by the target itself. A deploy never
/// assumes `$HOME` is the same string on both machines: a home with a space in it
/// is exactly the case a hardcoded `/home/user` gets wrong, and a home reached
/// through a symlink is exactly the case that makes a retired release path and
/// its real path disagree.
pub async fn remote_home(host: &str) -> Result<String, CommandFailure> {
    let outcome = exec(host, "set -e; cd ~ && pwd").await?;
    if !outcome.ok() {
        return Err(codes::refuse(
            SSH_UNREACHABLE,
            format!(
                "cannot resolve the remote home directory on {host}\n{}",
                outcome.detail()
            ),
        ));
    }
    let home = outcome.stdout.trim().to_string();
    reject_control_characters("home directory", &home)?;
    if !home.starts_with('/') {
        return Err(codes::refuse(
            codes::NO_REMOTE_RUNTIME,
            format!("{host} reported a home directory that is not absolute: {home}"),
        ));
    }
    Ok(home)
}

/// A command that reads one file on the target and prints it, for the installed
/// definition backfill. The path is quoted, and a file that is not there prints
/// nothing rather than failing: a fresh target has no definition yet, and that
/// is the normal state a first install starts from.
pub fn read_remote_file(path: &str) -> String {
    let quoted = posix_shell_quote(path);
    format!("if test -f {quoted}; then cat {quoted}; fi")
}
