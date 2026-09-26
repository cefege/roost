//! Holding a machine's transaction open across a whole deploy, from the
//! deploying box. Called by the deploy command and by keeper-refresh; depends on
//! the ssh transport and on the target-side transaction command's own output
//! vocabulary, and on nothing else in the deploy group.
//!
//! The lock is held by a remote process that blocks on its standard input, and
//! the deploying box closes that input when it is done. That shape is the whole
//! point: a lease the deploying box has to renew over a network is a lease that
//! can lapse mid-deploy, and a deploy that discovers its lease lapsed has to
//! decide whether the mutation it half performed is safe to continue — which is
//! the question the machine transaction exists to make unnecessary. Here the
//! kernel releases the lock the instant the remote process dies, however it
//! dies, and the apply on the far side refuses to run at all without it.
//!
//! The refusal is the important half: if this process is killed, the session's
//! stdin closes, the remote process exits, and the machine is free. There is no
//! window in which the machine believes it is serialized and is not.

use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::machine_txn::TransactionKind;
use crate::deploy::ssh::{REMOTE_PATH_PREFIX, SSH_OPTS};

/// The line the target prints once it holds the machine's transaction.
const HELD_PREFIX: &str = "RoostTransaction=held";

/// The line it prints once it has let the machine go.
const RELEASED_PREFIX: &str = "RoostTransaction=released";

/// A machine transaction held on a target for as long as this value lives.
#[derive(Debug)]
pub struct RemoteTransaction {
    child: Child,
    held_record: String,
}

impl RemoteTransaction {
    /// Take the transaction on `host` by running `remote_command` there and
    /// holding the remote process open.
    pub async fn acquire(host: &str, remote_command: &str) -> Result<Self, CommandFailure> {
        let mut child = spawn(host, remote_command)?;
        let stdout = child.stdout.take().ok_or_else(|| {
            codes::refuse(
                codes::SSH_UNREACHABLE,
                "the machine transaction command produced no output stream",
            )
        })?;
        // The first line is the answer; everything after it is the process
        // holding the lock, and this read is what makes the refusal fast rather
        // than a timeout minutes later.
        let mut lines = BufReader::new(stdout).lines();
        let first = lines.next_line().await.ok().flatten().unwrap_or_default();
        if !first.trim().starts_with(HELD_PREFIX) {
            let _ = child.start_kill();
            return Err(codes::refuse(
                codes::REMOTE_LOST,
                format!(
                    "cannot take the machine transaction on {host}\n{first}\nthe target must be \
                     running the release this deploy staged, with its service directory writable"
                ),
            ));
        }
        println!("   machine transaction held on {host}: {}", first.trim());
        Ok(Self {
            child,
            held_record: first.trim().to_string(),
        })
    }

    /// The record the target reported it took, for a deploy's own log line.
    pub fn record(&self) -> &str {
        &self.held_record
    }

    /// Give the machine back, and wait for the target to confirm it did.
    ///
    /// The confirmation is read rather than assumed because a release that is
    /// gone is not a release the next deploy can recover from, and "I closed the
    /// pipe" is not evidence that anything was released.
    pub async fn release(mut self) -> Result<(), CommandFailure> {
        drop(self.child.stdin.take());
        let output = self.child.wait_with_output().await.map_err(|error| {
            codes::refuse(
                codes::REMOTE_LOST,
                format!("the machine transaction on the target ended without an answer: {error}"),
            )
        })?;
        let text = String::from_utf8_lossy(&output.stdout).into_owned();
        if !text.contains(RELEASED_PREFIX) {
            return Err(codes::refuse(
                codes::REMOTE_LOST,
                format!(
                    "the machine transaction on the target was not released cleanly: {}",
                    if text.trim().is_empty() {
                        String::from_utf8_lossy(&output.stderr).trim().to_string()
                    } else {
                        text.trim().to_string()
                    }
                ),
            ));
        }
        Ok(())
    }
}

/// The command that makes the STAGED or INSTALLED release at `program` hold the
/// target's machine transaction.
///
/// The service directory and the journal path are resolved by the target from its
/// own environment rather than passed in: this box does not know them, and a path
/// guessed here would be a lock on a file the install does not use.
pub fn command_for(program: &str, kind: TransactionKind) -> String {
    format!(
        "{quoted} __remote-transaction --kind {kind}",
        quoted = roost_platform::posix_shell_quote(program),
        kind = kind.as_str(),
    )
}

fn spawn(host: &str, command: &str) -> Result<Child, CommandFailure> {
    let mut argv: Vec<String> = vec!["ssh".to_string()];
    argv.extend(SSH_OPTS.iter().map(|option| (*option).to_string()));
    argv.push("--".to_string());
    argv.push(host.to_string());
    argv.push(format!("{REMOTE_PATH_PREFIX}{command}"));
    Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| codes::refuse(codes::SSH_UNREACHABLE, format!("cannot run ssh: {error}")))
}
