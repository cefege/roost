//! The resolved shell launch contract: what a keeper PTY is opened with,
//! decided in full before anything reaches the keeper. `session::spawn` builds
//! one, `keeper_pool` sends it verbatim, and `session::types` retains it for a
//! respawn. Depends on `roost_host::HostPlatform` for the platform and
//! `roost_keeper::frames::ShellSpec` for the wire form — and on nothing here.
//!
//! The resolver that PRODUCES a spec from an environment is deliberately not in
//! this file: it materialises directories, reads `SHELL`, and writes a
//! bootstrap rcfile, which is I/O, and `session::types` has to be able to hold
//! a spec without pulling any of that in. `crate::host` owns the resolution.
//!
//! The contract is resolved BEFORE the spawn rather than applied by the keeper:
//! a spawn that fails has to fail with a reason the caller can read, and a
//! reason the keeper invents three frames later is a reason nobody sees.

use roost_host::HostPlatform;
use roost_keeper::frames::ShellSpec as KeeperShellSpec;

/// The version this build resolves and the keeper understands.
///
/// A literal rather than an enum because the keeper's contract version is a
/// number on the wire, and a spec that named a variant would still have to
/// carry it.
pub const SHELL_SPEC_VERSION: u32 = 1;

/// The environment variable carrying the session id into the PTY.
///
/// Present in the PTY's own environment, not only the record, because a shell
/// script the user writes needs to know which session it is running inside.
pub const SESSION_ID_ENV: &str = "ROOST_SESSION_ID";

/// The prefix every keeper control credential carries.
///
/// Keeper control credentials are worker/keeper-only and MUST NOT reach a PTY.
/// A worker that leaks one hands any command the user runs the ability to
/// speak to the keeper as this worker, which is the whole machine's terminals.
pub const KEEPER_CONTROL_ENV_PREFIX: &str = "ROOST_KEEPER_";

/// Whether a key names a keeper control credential and so may not be inherited
/// into a PTY.
pub fn is_keeper_control_key(key: &str) -> bool {
    key.to_ascii_uppercase()
        .starts_with(KEEPER_CONTROL_ENV_PREFIX)
}

/// A fully resolved, serialisable shell launch contract.
///
/// `argv` holds arguments only; `executable` is passed separately to the
/// process boundary, because a spec that put the program in `argv[0]` would give
/// the keeper two places to disagree about what runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSpec {
    pub version: u32,
    pub platform: HostPlatform,
    pub executable: String,
    pub argv: Vec<String>,
    /// The folder the PTY is OPENED in. Never the shell's later `cwd`.
    pub cwd: String,
    /// The PTY's whole environment, in a deterministic order.
    ///
    /// A map in v2 and a vector here because the keeper's wire form is a
    /// sequence, and a `HashMap` in between would make two resolutions of the
    /// same request produce two different byte streams for no reason.
    pub env: Vec<(String, String)>,
}

impl ShellSpec {
    /// This contract as the keeper's own wire shape.
    ///
    /// The keeper executes what this says verbatim and applies no policy of its
    /// own, so the conversion is total and lossy in no field: the version and
    /// platform are the worker's to check, not the keeper's to read.
    pub fn keeper_command(&self) -> KeeperShellSpec {
        KeeperShellSpec {
            program: self.executable.clone(),
            args: self.argv.clone(),
            env: self.env.clone(),
            cwd: Some(self.cwd.clone()),
        }
    }

    /// The value of one environment variable, matched case-insensitively on
    /// Windows only — which is the one platform whose environment names are not
    /// case-sensitive, and which v3 does not port.
    pub fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}
