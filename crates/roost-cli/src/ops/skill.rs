//! `roost skill` — print the release-matched agent skill, with no decoration.
//! Called by the crate's dispatcher. The document is embedded at compile time
//! rather than read from a path: a compiled binary has no source tree to
//! resolve `skills/roost/SKILL.md` against, and a command whose output depends
//! on the working directory is not a command an agent can rely on.
//!
//! `include_str!` is also what makes the pairing honest — the skill a released
//! binary prints is the skill that was in the tree at that commit, not whatever
//! the checkout on the machine happens to hold now.

use std::process::ExitCode;

use clap::Args;

use crate::command_error::CommandFailure;

/// The document, embedded. The path is relative to this file and is checked by
/// the compiler, so a moved or deleted document is a build failure rather than
/// a command that prints nothing to an agent that trusted it.
const ROOST_SKILL: &str = include_str!("../../../../skills/roost/SKILL.md");

#[derive(Debug, Args)]
#[command(about = "Print the exact release-matched ROOST agent skill")]
pub struct SkillArgs {}

pub fn run(_args: &SkillArgs) -> Result<ExitCode, CommandFailure> {
    print!("{ROOST_SKILL}");
    Ok(ExitCode::SUCCESS)
}

/// The document, for a caller that wants it as a value. Public because the
/// byte-for-byte shape is part of the contract and a test asserts it.
pub fn skill_text() -> &'static str {
    ROOST_SKILL
}
