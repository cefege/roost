//! `roost version [--build]` — one bare token, and the identity behind it.
//! Called by the crate's dispatcher. Reads `roost-host`'s build identity, which
//! is the one place a compiled stamp and a source checkout's environment stamp
//! are reconciled, so this command has no identity logic of its own.
//!
//! One token and nothing else is the contract. `roost push` captures this
//! output as the SHA a release proves, and a decorated line would have to be
//! parsed back apart by every reader.

use std::process::ExitCode;

use clap::Args;
use roost_host::ProcessEnv;

use crate::command_error::CommandFailure;

#[derive(Debug, Args)]
#[command(about = "Print the roost version, or the build SHA with --build")]
pub struct VersionArgs {
    #[arg(long)]
    pub build: bool,
}

pub fn run(args: &VersionArgs) -> Result<ExitCode, CommandFailure> {
    let identity = roost_host::build_identity(&ProcessEnv::new());
    if args.build {
        println!("{}", identity.build_sha);
    } else {
        println!("{}", identity.artifact_version);
    }
    Ok(ExitCode::SUCCESS)
}
