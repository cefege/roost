//! The exit codes `roost deploy` and `roost keeper-refresh` are allowed to
//! return, named once so no call site invents a number. Called by every module
//! in the deploy group and by the crate's dispatcher.
//!
//! These are not preferences. `roost push` shares 5, 7 and 8 with this command
//! so a wrapper can tell "refused, and do not retry" from "failed, try again",
//! and `roost push` rolls the whole fleet back on a settlement failure. A code
//! that means something different in one of the two is a wrapper that rolls back
//! a fleet for the wrong reason.
//!
//! The one collision is deliberate and recorded in
//! `docs/phase6-cli-contract.md`: 2 is clap's usage code for a bad flag AND
//! "ssh failed", exactly as it was in v2.

use crate::command_error::CommandFailure;

/// The operator's own invocation is wrong: no host, a malformed `--label`, a
/// `--source-root` that is not a path. Distinct from clap's 2, which covers a
/// flag the parser itself rejects.
pub const USAGE: u8 = 1;

/// The target could not be reached, or answered nothing. Also clap's own code
/// for a rejected invocation, which is why it is named for the transport and
/// not for the syntax.
pub const SSH_UNREACHABLE: u8 = 2;

/// The target has no runtime that can run the release: not a POSIX platform,
/// or an architecture the release was not built for.
pub const NO_REMOTE_RUNTIME: u8 = 3;

/// The release this deploy would ship could not be built here.
pub const BUILD_FAILED: u8 = 4;

/// A keeper could not be adopted safely, and the deploy stopped without
/// touching it. The only code that means "a person's PTYs are at stake".
pub const KEEPER_NOT_ADOPTABLE: u8 = 5;

/// The target has no coordinator URL and no prior install to reuse one from.
pub const NO_COORDINATOR_URL: u8 = 6;

/// The build identity could not be proved: a dirty tree, an unpushed commit, or
/// a checkout that is not the release it claims to be.
pub const IDENTITY_UNPROVED: u8 = 7;

/// The new definition came up and the deploy reached its irreversible point,
/// and the release could not be settled after it.
pub const SETTLEMENT_FAILED: u8 = 8;

/// A remote process died, or the machine transaction on the target was lost,
/// while the deploy held it.
pub const REMOTE_LOST: u8 = 9;

/// A refusal carrying one of the reserved codes, with the message an operator
/// reads on stderr.
pub fn refuse(code: u8, message: impl Into<String>) -> CommandFailure {
    CommandFailure::new(code, message)
}
