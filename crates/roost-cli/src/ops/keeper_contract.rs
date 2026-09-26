//! `roost __keeper-contract` — the hidden release-artifact probe. Prints the
//! keeper ABI this release would ship, as one line of JSON, and takes no
//! arguments. Called by the crate's dispatcher.
//!
//! It exists because update admission runs the DOWNLOADED candidate before
//! replacing itself: a keeper's contract has to come from the exact target
//! runtime, not from the binary that happens to be installed while the decision
//! is being made. The name keeps its leading underscores because the deploy and
//! upgrade paths address it by that exact string.
//!
//! Every field is the keeper crate's, including the digest of the
//! `roost-keeper` binary — which is a DIFFERENT file from this one. Reading
//! `current_exe()` here would report the digest of `roost`, and every admission
//! decision made from this output would be about the wrong program.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;

use crate::command_error::CommandFailure;

#[derive(Debug, Args)]
#[command(
    name = "__keeper-contract",
    about = "Print this release's keeper ABI as JSON"
)]
pub struct KeeperContractArgs {}

pub fn run(_args: &KeeperContractArgs) -> Result<ExitCode, CommandFailure> {
    // Built by the keeper crate, from the keeper crate's own answers to "what
    // protocol is this" and "which features does it have" — not restated here,
    // because a second copy of that table is a second answer.
    let mut contract = roost_keeper::keeper::Keeper::new().contract().clone();
    // …except the digest, which `Keeper::new` computes over `current_exe()`. In
    // THIS process that is `roost`, not the keeper, and admission compares this
    // value against a running keeper's. So it is recomputed over the sibling
    // binary this release actually ships.
    contract.implementation_digest =
        roost_keeper::keeper::implementation_digest_of(&keeper_binary_path()?);
    let encoded = serde_json::to_string(&contract)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    println!("{encoded}");
    Ok(ExitCode::SUCCESS)
}

/// The `roost-keeper` binary that ships beside this one. A release puts both
/// in the same directory and the installer links them from there, so the
/// sibling is the artifact a deploy will actually start.
pub fn keeper_binary_path() -> Result<PathBuf, CommandFailure> {
    let current = std::env::current_exe()
        .map_err(|error| CommandFailure::generic(format!("cannot locate this binary: {error}")))?;
    let directory = current.parent().ok_or_else(|| {
        CommandFailure::generic(format!(
            "this binary has no parent directory: {}",
            current.display()
        ))
    })?;
    let candidate = directory.join("roost-keeper");
    if !candidate.is_file() {
        return Err(CommandFailure::generic(format!(
            "no roost-keeper binary beside {}; a keeper contract describes a keeper this \
             release does not ship",
            current.display()
        )));
    }
    Ok(candidate)
}
