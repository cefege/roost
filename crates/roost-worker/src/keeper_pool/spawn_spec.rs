//! The one command a PTY is opened with: the resolved spec with every keeper
//! control credential withheld. `pool::KeeperPool::spawn` sends exactly this,
//! and nothing else builds one, so this file is the last place a credential can
//! be stopped. Depends on `crate::shell_spec` for the spec and the capability
//! predicate, and on `roost_keeper::frames` for the wire shape — nothing here.
//!
//! THE SECURITY SURFACE. The capability check, the credential stripping and the
//! controlling-TTY handshake the keeper performs are ONE property: a PTY must
//! never hold a credential that speaks to the keeper as this worker. A worker
//! that leaks one hands every command a user types the ability to drive every
//! terminal on the machine. So the stripping is not a convenience applied where
//! convenient — it is applied at the boundary, on the resolved spec, whatever
//! produced it, because the resolver that built the spec is a different module
//! with a different reason to be wrong.
//!
//! The predicate is case-insensitive on purpose. A case-SENSITIVE check smuggles
//! a credential into a PTY under any mixed-case spelling, and every command the
//! user runs afterwards inherits it.

use roost_keeper::frames::ShellSpec as KeeperShellSpec;

use crate::shell_spec::{ShellSpec, is_keeper_control_key};

/// A PTY's command, and the credentials this boundary refused to pass on.
#[derive(Clone, PartialEq, Eq)]
pub struct PtyCommand {
    /// What the keeper executes, verbatim.
    pub command: KeeperShellSpec,
    /// The environment names withheld, by name, so the refusal is reportable.
    ///
    /// Reported rather than swallowed because a spec that carried one means the
    /// resolver let something through, and an operator reading the log is the
    /// only one who can tell which layer did it.
    pub withheld: Vec<String>,
}

impl PtyCommand {
    /// Whether this boundary had to withhold anything.
    pub fn withheld_any(&self) -> bool {
        !self.withheld.is_empty()
    }
}

impl std::fmt::Debug for PtyCommand {
    /// Prints the program and the count, never the environment: a log line that
    /// dumps a PTY's environment is a log line that can print a credential.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PtyCommand")
            .field("program", &self.command.program)
            .field("args", &self.command.args)
            .field("cwd", &self.command.cwd)
            .field("env_entries", &self.command.env.len())
            .field("withheld", &self.withheld)
            .finish()
    }
}

/// The command for a PTY, with every keeper control credential removed.
///
/// Total, and it cannot fail: a spec with no credentials in it is the ordinary
/// case, and a refusal here would cost a terminal to defend against a leak
/// that a filtered environment already prevents.
pub fn pty_command(spec: &ShellSpec) -> PtyCommand {
    let mut withheld = Vec::new();
    let mut sanitized = spec.clone();
    sanitized.env.retain(|(key, _)| {
        if is_keeper_control_key(key) {
            withheld.push(key.clone());
            false
        } else {
            true
        }
    });
    PtyCommand {
        command: sanitized.keeper_command(),
        withheld,
    }
}
