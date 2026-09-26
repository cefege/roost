//! The `roost` binary's command tree and subcommand dispatch, held in a
//! library so the argument parser is testable without spawning a process.
//!
//! The binary's stdout is this program's product surface, which is why
//! roost-cli is the one crate `cargo xtask lint` exempts from the no-stdout
//! rule; every other crate logs through roost-observability. That exemption is
//! a statement, not a loophole: `println!` here is for something a person runs
//! and reads, and anything a machine reads is either a single token
//! (`version`), one line of JSON (`__keeper-contract`), or a documented block
//! of text (`status`, `doctor`) whose exact shape is pinned by a test in this
//! crate. A log line does not belong on this side of the line, and
//! `docs/phase6-cli-contract.md` says per command which side it is on.
//!
//! Every subcommand's arguments, exit codes and output shape are recorded in
//! that document, and the exit codes are part of the contract: a script that
//! cannot tell a "refused, and do not retry" from a "failed, try again" is a
//! script that retries the one thing that must never be retried.

pub mod command_error;
pub mod daemon;
pub mod deploy;
pub mod doctor;
pub mod ops;
pub mod overlay_env;
pub mod services;
pub mod status;
pub mod utc_clock;
pub mod wall_clock;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::command_error::CommandFailure;
use crate::daemon::{CoordArgs, KeeperArgs, WorkerArgs};
use crate::deploy::remote_commands::{
    RemoteApplyArgs, RemoteEvidenceArgs, RemoteFactsArgs, RemoteTransactionArgs,
};
use crate::deploy::{DeployArgs, KeeperRefreshArgs};
use crate::doctor::DoctorArgs;
use crate::ops::keeper_contract::KeeperContractArgs;
use crate::ops::logs::LogsArgs;
use crate::ops::reset::ResetArgs;
use crate::ops::skill::SkillArgs;
use crate::ops::state::StateArgs;
use crate::ops::test::TestArgs;
use crate::ops::version::VersionArgs;
use crate::status::StatusArgs;

#[derive(Debug, Parser)]
#[command(
    name = "roost",
    about = "Roost operator CLI",
    // A bare `roost` with no subcommand is a usage error, not a help screen:
    // an operator who typed the wrong word should be told, not handed a page
    // they have to read to find out which word was right.
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run the coordinator in this process (server mode).
    Coord(CoordArgs),
    /// Run the worker in this process (server mode).
    Worker(WorkerArgs),
    /// Run the keeper in this process (the self-exec target).
    Keeper(KeeperArgs),
    /// Health readout: local services, coordinator, front door, workers.
    Status(StatusArgs),
    /// Anomaly digest from this host's logs and the coordinator's audit log.
    Doctor(DoctorArgs),
    /// Print the roost version, or the build SHA with --build.
    Version(VersionArgs),
    /// Tail a service's logs.
    Logs(LogsArgs),
    /// Deploy this build to one machine's worker over ssh.
    Deploy(DeployArgs),
    /// Shut a machine's keeper down empty, keeping its worker installed.
    KeeperRefresh(KeeperRefreshArgs),
    #[command(name = "__remote-facts", hide = true)]
    RemoteFacts(RemoteFactsArgs),
    #[command(name = "__remote-evidence", hide = true)]
    RemoteEvidence(RemoteEvidenceArgs),
    #[command(name = "__remote-transaction", hide = true)]
    RemoteTransaction(RemoteTransactionArgs),
    #[command(name = "__remote-apply", hide = true)]
    RemoteApply(RemoteApplyArgs),
    /// Print a STATE.md snapshot of this checkout.
    State(StateArgs),
    /// Stop the local services and delete the coordinator database.
    Reset(ResetArgs),
    /// Print the exact release-matched ROOST agent skill.
    Skill(SkillArgs),
    /// Run a test profile.
    Test(TestArgs),
    #[command(name = "__keeper-contract", hide = true)]
    KeeperContract(KeeperContractArgs),
}

impl Command {
    /// The name the failure line reports. It is the subcommand as typed, so a
    /// `{"cmd":…}` line in a log points at the command an operator ran.
    pub fn name(&self) -> &'static str {
        match self {
            Command::Coord(_) => "coord",
            Command::Worker(_) => "worker",
            Command::Keeper(_) => "keeper",
            Command::Status(_) => "status",
            Command::Doctor(_) => "doctor",
            Command::Version(_) => "version",
            Command::Logs(_) => "logs",
            Command::Deploy(_) => "deploy",
            Command::KeeperRefresh(_) => "keeper-refresh",
            Command::RemoteFacts(_) => "__remote-facts",
            Command::RemoteEvidence(_) => "__remote-evidence",
            Command::RemoteTransaction(_) => "__remote-transaction",
            Command::RemoteApply(_) => "__remote-apply",
            Command::State(_) => "state",
            Command::Reset(_) => "reset",
            Command::Skill(_) => "skill",
            Command::Test(_) => "test",
            Command::KeeperContract(_) => "__keeper-contract",
        }
    }
}

/// Run one parsed command and hand back the code to exit with. A failure keeps
/// its own code, so `main.rs` has nothing to decide.
pub async fn dispatch(cli: Cli) -> Result<ExitCode, CommandFailure> {
    match cli.command {
        Command::Coord(args) => daemon::run_coord(&args).await,
        Command::Worker(args) => daemon::run_worker(&args).await,
        Command::Keeper(args) => daemon::run_keeper(&args),
        Command::Status(args) => status::run(&args).await,
        Command::Doctor(args) => doctor::run(&args).await,
        Command::Version(args) => ops::version::run(&args),
        Command::Logs(args) => ops::logs::run(&args),
        Command::Deploy(args) => deploy::run::run(&args).await,
        Command::KeeperRefresh(args) => deploy::keeper_refresh::run(&args).await,
        Command::RemoteFacts(args) => deploy::remote_commands::facts(&args),
        Command::RemoteEvidence(args) => deploy::remote_commands::evidence(&args),
        Command::RemoteTransaction(args) => deploy::remote_commands::transaction(&args).await,
        Command::RemoteApply(args) => deploy::remote_commands::apply(&args).await,
        Command::State(args) => ops::state::run(&args),
        Command::Reset(args) => ops::reset::run(&args),
        Command::Skill(args) => ops::skill::run(&args),
        Command::Test(args) => ops::test::run(&args),
        Command::KeeperContract(args) => ops::keeper_contract::run(&args),
    }
}
