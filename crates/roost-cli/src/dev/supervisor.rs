//! The fan-out: start every child, notice when one of them exits, and stop the
//! rest. Called by `dev::run`, and driven directly by the tests with children
//! that are not the dev servers — a fan-out only ever proven against a real
//! coordinator proves nothing about what Ctrl-C does.
//!
//! Everything here is about the PROCESS, not about a server: a dev stack that
//! leaves a coordinator holding its port, or a keeper holding its PTYs, is the
//! defect this module exists to prevent.

use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};
use tokio::signal::unix::{Signal, SignalKind, signal as unix_signal};

use crate::command_error::CommandFailure;
use crate::dev::plan::DevServer;
use crate::dev::signal::{self, SignalError};

/// How often the children are asked whether they are still running. `wait`
/// borrows the child it waits for, and three of those cannot be raced in one
/// `select!` without boxing a future per child; a dev supervisor does not need
/// better than the interval.
pub const EXIT_POLL: Duration = Duration::from_millis(50);

/// How long a stack waits for a child to leave, and how a caller asks for a
/// different wait. A test drives a short grace so the escalation after it is
/// observable without a five-second test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopPolicy {
    pub grace: Duration,
}

impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(5),
        }
    }
}

/// The signal that reaches a child, and the one this process was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationSignal {
    Interrupt,
    Terminate,
}

impl TerminationSignal {
    pub fn name(self) -> &'static str {
        match self {
            Self::Interrupt => signal::INTERRUPT,
            Self::Terminate => signal::TERMINATE,
        }
    }
}

/// A child that ended, by code or by signal. A signalled child reports no code
/// at all, which is why this is an `Option` rather than a zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerExit {
    pub name: &'static str,
    pub code: Option<i32>,
}

#[derive(Debug)]
struct RunningServer {
    name: &'static str,
    child: Child,
    /// Set the moment the child has been reaped. A pid the kernel has already
    /// handed to somebody else must never be signalled, and this is the only
    /// thing in the process that knows the pid is dead.
    reaped: bool,
}

/// The running dev stack. Dropping it kills whatever is left, so a panic or a
/// cancelled command cannot leave a child behind either.
#[derive(Debug)]
pub struct DevStack {
    running: Vec<RunningServer>,
    policy: StopPolicy,
}

impl DevStack {
    /// Start every server, in order. A server that cannot be started stops the
    /// ones already running before the failure leaves: a half-started stack is
    /// the state this command exists to prevent, and the operator gets one
    /// failure line naming the child and the reason rather than a stack that
    /// quietly lost a third of itself.
    pub async fn start(servers: &[DevServer], policy: StopPolicy) -> Result<Self, CommandFailure> {
        let mut stack = Self {
            running: Vec::with_capacity(servers.len()),
            policy,
        };
        for server in servers {
            if let Err(failure) = stack.spawn(server) {
                if let Err(cleanup) = stack.stop(TerminationSignal::Interrupt).await {
                    tracing::error!(%cleanup, "the dev stack could not be cleaned up after a failed start");
                }
                return Err(failure);
            }
        }
        tracing::info!(
            servers = stack.running.len(),
            "dev stack started; every child owns this terminal's stdio"
        );
        Ok(stack)
    }

    /// Wait until one child has exited, and say which. `None` means there was
    /// nothing left to wait for.
    pub async fn wait_for_exit(&mut self) -> Option<ServerExit> {
        loop {
            if let Some(exit) = self.reap_exited() {
                return Some(exit);
            }
            if self.is_down() {
                return None;
            }
            tokio::time::sleep(EXIT_POLL).await;
        }
    }

    /// Stop every child that is still running, and wait for it to be gone.
    ///
    /// The failure is reported only when a child outlived both the polite
    /// signal and SIGKILL, because that is the one case where this process is
    /// about to exit with something still attached to a port or a PTY.
    pub async fn stop(&mut self, forwarded: TerminationSignal) -> Result<(), CommandFailure> {
        self.signal_the_live(forwarded.name());
        if self.await_down(self.policy.grace).await {
            tracing::info!("dev stack stopped");
            return Ok(());
        }
        tracing::warn!(
            grace_ms = self.policy.grace.as_millis(),
            "a dev server did not leave after the polite signal; killing it"
        );
        self.signal_the_live(signal::KILL);
        if self.await_down(self.policy.grace).await {
            tracing::info!("dev stack stopped after the escalation");
            return Ok(());
        }
        let stuck: Vec<&str> = self
            .running
            .iter()
            .filter(|server| !server.reaped)
            .map(|server| server.name)
            .collect();
        Err(CommandFailure::generic(format!(
            "the {} did not stop after {} and after {}",
            stuck.join(", "),
            forwarded.name(),
            signal::KILL,
        )))
    }

    fn spawn(&mut self, server: &DevServer) -> Result<(), CommandFailure> {
        let mut child = Command::new(&server.program)
            .args(&server.args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                let program = server.program.display();
                CommandFailure::generic(format!("{}: {program}: {error}", server.name))
            })?;
        tracing::info!(
            server = server.name,
            pid = ?child.id(),
            command = server.command_line(),
            "dev server started"
        );
        self.running.push(RunningServer {
            name: server.name,
            child,
            reaped: false,
        });
        Ok(())
    }

    /// Collect every child that has exited since the last look, and report the
    /// first of them.
    fn reap_exited(&mut self) -> Option<ServerExit> {
        let mut first = None;
        for server in &mut self.running {
            if server.reaped {
                continue;
            }
            let ended = match server.child.try_wait() {
                Ok(Some(status)) => Some(status.code()),
                Ok(None) => None,
                Err(error) => {
                    // The handle cannot be asked again, so this child counts as
                    // gone: signalling a pid nobody can reap is how a shutdown
                    // kills an unrelated process.
                    tracing::warn!(server = server.name, %error, "cannot wait for a dev server");
                    Some(None)
                }
            };
            let Some(exit_code) = ended else { continue };
            server.reaped = true;
            tracing::info!(server = server.name, ?exit_code, "dev server exited");
            first.get_or_insert(ServerExit {
                name: server.name,
                code: exit_code,
            });
        }
        first
    }

    fn signal_the_live(&mut self, name: &'static str) {
        for server in self.running.iter_mut().filter(|server| !server.reaped) {
            let Some(pid) = server.child.id() else {
                continue;
            };
            match signal::send(pid, name) {
                Ok(()) => {
                    tracing::info!(
                        server = server.name,
                        pid,
                        signal = name,
                        "dev server signalled"
                    )
                }
                // A refusal is usually a child that exited between the look and
                // the signal, so it is a warning and not a failure.
                Err(SignalError::Refused { .. }) => {
                    tracing::warn!(
                        server = server.name,
                        pid,
                        signal = name,
                        "dev server took no signal"
                    )
                }
                Err(failure) => {
                    tracing::error!(server = server.name, %failure, "dev server not signalled")
                }
            }
        }
    }

    /// Poll until nothing is running, or the deadline passes. Whether the stack
    /// is down is the answer; the exits themselves are already logged.
    async fn await_down(&mut self, within: Duration) -> bool {
        let deadline = Instant::now() + within;
        loop {
            let _ = self.reap_exited();
            if self.is_down() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(EXIT_POLL).await;
        }
    }

    fn is_down(&self) -> bool {
        self.running.iter().all(|server| server.reaped)
    }
}

/// The two signals a terminal or a service manager sends this process, held
/// open for as long as the stack runs. Installed BEFORE anything is started, so
/// a Ctrl-C during startup is caught rather than ending the process with
/// children already attached.
#[derive(Debug)]
pub struct TerminationWatch {
    interrupt: Signal,
    terminate: Signal,
}

impl TerminationWatch {
    pub fn install() -> Result<Self, CommandFailure> {
        let interrupt = unix_signal(SignalKind::interrupt())
            .map_err(|error| CommandFailure::generic(error.to_string()))?;
        let terminate = unix_signal(SignalKind::terminate())
            .map_err(|error| CommandFailure::generic(error.to_string()))?;
        Ok(Self {
            interrupt,
            terminate,
        })
    }

    /// The next signal, or `None` when this process can no longer be signalled
    /// at all. A caller treats that as a reason to stop the stack, not as a
    /// reason to leave it running with nobody watching.
    pub async fn next(&mut self) -> Option<TerminationSignal> {
        tokio::select! {
            received = self.interrupt.recv() => received.map(|()| TerminationSignal::Interrupt),
            received = self.terminate.recv() => received.map(|()| TerminationSignal::Terminate),
        }
    }
}
