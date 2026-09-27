//! The four hidden commands the TARGET side of a deploy runs on the machine
//! being changed. Called by the crate's dispatcher and by nothing else; depends
//! on the deploy group's own modules, and on nothing outside this crate.
//!
//! Each answers exactly one question, and each is hidden because an operator
//! never types them — a deploy addresses them by string over ssh, the same way
//! it addresses `roost __keeper-contract`. Keeping them to one question each is
//! what lets the deploying box hold its side of the boundary: it knows what to
//! ask, and every answer comes from the release it just staged rather than from
//! a path it guessed.
//!
//! | Command | Question | Answers with |
//! | --- | --- | --- |
//! | `__remote-facts` | what is installed here? | one line of JSON |
//! | `__remote-evidence` | may a release be staged? | the evidence markers |
//! | `__remote-transaction` | hold the machine still | a line, then blocks |
//! | `__remote-apply` | install this release | one line of JSON |

use std::io::Read as _;
use std::process::ExitCode;

use clap::Args;

use crate::command_error::CommandFailure;
use crate::deploy::apply_release::{manifest_from_stdin, process_environment};
use crate::deploy::machine_txn::{self, TransactionKind};

/// `roost __remote-facts` — what this machine has installed.
#[derive(Debug, Args)]
#[command(
    name = "__remote-facts",
    about = "Report this machine's installed worker identity and paths"
)]
pub struct RemoteFactsArgs {}

/// Run the facts probe for the command that spawned this process.
pub fn facts(_args: &RemoteFactsArgs) -> Result<ExitCode, CommandFailure> {
    let platform = roost_host::supported_host_platform()?;
    let facts = crate::deploy::facts::read(process_environment(), platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let encoded = crate::deploy::facts::encode(&facts)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    println!("{}{encoded}", crate::deploy::facts::FACTS_PREFIX);
    Ok(ExitCode::SUCCESS)
}

/// `roost __remote-evidence` — may a release be staged on this machine?
#[derive(Debug, Args)]
#[command(
    name = "__remote-evidence",
    about = "Report whether a staged release could destroy anything here"
)]
pub struct RemoteEvidenceArgs {}

/// Run the evidence probe: one command on this machine, and the markers that
/// answer whether a staged release has anything to destroy.
pub fn evidence(_args: &RemoteEvidenceArgs) -> Result<ExitCode, CommandFailure> {
    let env = process_environment();
    let platform = roost_host::supported_host_platform()?;
    let label = crate::services::service_spec::ServiceRole::Worker
        .service_label(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let spec = crate::services::service_spec::ServiceRole::Worker
        .definition_path(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let command = crate::deploy::target_evidence::target_worker_evidence_command(
        platform,
        &label,
        &spec.display().to_string(),
    );
    // Inherited stdio rather than a captured run: the markers have to reach the
    // deploying box on this process's stdout, and the service manager's own
    // report is part of what the markers are parsed out of.
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .status()
        .map_err(|error| {
            CommandFailure::generic(format!("cannot run the evidence probe: {error}"))
        })?;
    Ok(ExitCode::from(status.code().unwrap_or(1) as u8))
}

/// `roost __remote-transaction` — hold this machine still for another deploy.
#[derive(Debug, Args)]
#[command(
    name = "__remote-transaction",
    about = "Hold this machine's transaction until standard input closes"
)]
pub struct RemoteTransactionArgs {
    /// What the holder is doing, recorded so the next operator can read it.
    #[arg(long)]
    kind: String,
}

/// Take the machine transaction, say so, and hold it until stdin closes.
///
/// The wait is the whole mechanism: closing this process's input is how the
/// holder says it is done, and the kernel releasing the file lock is how the
/// machine notices when the holder dies without saying anything.
pub async fn transaction(args: &RemoteTransactionArgs) -> Result<ExitCode, CommandFailure> {
    let kind = TransactionKind::parse(&args.kind).map_err(CommandFailure::usage)?;
    let env = process_environment();
    let platform = roost_host::supported_host_platform()?;
    let service_dir = roost_host::roost_service_dir(env, platform)
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let journal = crate::services::deploy_journal::DeployJournal::path_in(&service_dir);
    let transaction = machine_txn::MachineTransaction::acquire(
        &machine_txn::lock_path(&service_dir),
        kind,
        &journal,
        crate::wall_clock::now_ms(),
    )
    .await
    .map_err(|error| {
        CommandFailure::generic(format!("cannot take this machine's transaction: {error}"))
    })?;
    println!(
        "RoostTransaction=held kind={} pid={} epoch={}",
        kind.as_str(),
        transaction.record().owner_pid,
        transaction.record().process_epoch
    );
    wait_for_release().await;
    transaction
        .release()
        .await
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    println!("RoostTransaction=released");
    Ok(ExitCode::SUCCESS)
}

/// Block until the holder closes this process's standard input.
///
/// Read on a blocking thread rather than through tokio's stdin, which is behind a
/// feature this crate does not enable: a transaction holder is a process whose
/// only job is to wait, and the thread it waits on is idle by construction.
async fn wait_for_release() {
    let _ = tokio::task::spawn_blocking(|| {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0_u8; 64];
        loop {
            match stdin.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
        }
    })
    .await;
}

/// `roost __remote-apply` — install the release this manifest names.
#[derive(Debug, Args)]
#[command(
    name = "__remote-apply",
    about = "Install the release an apply manifest names, and report what happened"
)]
pub struct RemoteApplyArgs {}

/// Apply the manifest on standard input, and print the report the deploying box
/// reads.
pub async fn apply(_args: &RemoteApplyArgs) -> Result<ExitCode, CommandFailure> {
    let manifest = manifest_from_stdin()?;
    let report = crate::deploy::apply::run_here(&manifest).await;
    let encoded = report
        .encode()
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    println!("{}{encoded}", crate::deploy::manifest::REPORT_PREFIX);
    // The report is the answer and the exit code is the operator's: a refused
    // apply and a rolled-back apply are both "the report says so", and a caller
    // that treated a non-zero exit as "no report" would have to guess.
    Ok(ExitCode::SUCCESS)
}
