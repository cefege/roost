//! The operator commands that read a repository or a service definition and
//! print the result: `version`, `logs`, `state`, `reset`, `skill`, `test`, and
//! the hidden `__keeper-contract` probe. Each is its own file and owns its own
//! argument parsing and output; this module only names them.
//!
//! Every one of these prints something a person asked for by running it, which
//! is why `println!` is right here and `tracing` is wrong. The line between the
//! two is not "stdout vs stderr" but "the operator's screen vs the operator's
//! later self": a line about what this command is doing right now belongs on
//! the screen, and a line about what the fleet did overnight belongs in the
//! log `roost doctor` reads.

pub mod keeper_contract;
pub mod logs;
pub mod reset;
pub mod skill;
pub mod state;
pub mod test;
pub mod version;
