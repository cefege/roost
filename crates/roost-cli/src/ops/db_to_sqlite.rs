//! `roost db-to-sqlite` — copy a Postgres coordinator database into a SQLite
//! file, the reverse of `roost db-to-postgres`, so a coordinator can move back
//! to a file-backed install. Called by the crate dispatcher; the copy itself is
//! `roost_coord::db::postgres_to_sqlite`.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use roost_coord::db::postgres_to_sqlite::copy_postgres_to_sqlite;
use roost_coord::db::sqlite_to_postgres::{ExistingRows, TransferError, TransferReport};
use roost_host::coord_config_loader::{ENV_COORDINATOR_DATABASE_URL, ENV_COORDINATOR_DB};
use roost_host::{EnvSource, HostPlatform, ProcessEnv, coord_service_label};

use crate::command_error::CommandFailure;
use crate::import_v2::{CoordinatorState, coordinator_database, coordinator_state};
use crate::ops::db_to_postgres::{installed_sqlite_file, same_file, target_url};

/// `roost db-to-sqlite`.
#[derive(Debug, Args)]
#[command(about = "Copy a Postgres coordinator database into a SQLite file")]
pub struct DbToSqliteArgs {
    /// The source `postgres://` URL. Default: `ROOST_COORDINATOR_DATABASE_URL`
    /// from this shell, which keeps the password out of the process list.
    #[arg(long, value_name = "URL")]
    pub from: Option<String>,
    /// The SQLite file to write. Default: the file this host's installed
    /// coordinator declares, else the default data directory's.
    #[arg(long, value_name = "PATH")]
    pub to: Option<PathBuf>,
    /// Empty every coordinator table in the target before copying, instead of
    /// refusing a target that already holds rows.
    #[arg(long)]
    pub replace: bool,
}

/// Run the command.
pub async fn run(args: &DbToSqliteArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let source = target_url(args.from.as_deref(), &env, "--from")?;
    let installed_target = installed_sqlite_file(&env, platform);
    let target = match &args.to {
        Some(path) => path.clone(),
        None => match &installed_target {
            Some(path) => path.clone(),
            None => coordinator_database(&env, platform)?,
        },
    };
    refuse_if_target_is_live(&env, platform, &target, installed_target.as_deref())?;

    let existing = if args.replace {
        ExistingRows::Replace
    } else {
        ExistingRows::Refuse
    };
    println!("copying Postgres into {}…", target.display());
    let report = copy_postgres_to_sqlite(&source, &target, existing)
        .await
        .map_err(|error| transfer_failure(&error))?;
    print_report(&report, &target);
    Ok(ExitCode::SUCCESS)
}

/// A running coordinator on the target file would race the copy's writes.
fn refuse_if_target_is_live(
    env: &dyn EnvSource,
    platform: HostPlatform,
    target: &std::path::Path,
    installed_target: Option<&std::path::Path>,
) -> Result<(), CommandFailure> {
    let Some(installed_target) = installed_target else {
        return Ok(());
    };
    if !same_file(target, installed_target) {
        return Ok(());
    }
    let label = coord_service_label(env, platform)?;
    match coordinator_state(platform, &label) {
        CoordinatorState::Running => Err(CommandFailure::usage(format!(
            "the coordinator ({label}) is running against {}; stop it first \
             (`systemctl --user stop {label}` or `launchctl bootout`), then run this again.",
            target.display()
        ))),
        CoordinatorState::Stopped | CoordinatorState::Unavailable => Ok(()),
    }
}

/// A refusal the operator can fix is exit 2; anything else is a failure.
fn transfer_failure(error: &TransferError) -> CommandFailure {
    match error {
        TransferError::TargetNotEmpty { .. } => CommandFailure::usage(format!(
            "{error}. Pass --replace to empty it first; stop any coordinator using it."
        )),
        _ => CommandFailure::generic(format!("nothing was written to the SQLite file: {error}")),
    }
}

fn print_report(report: &TransferReport, target: &std::path::Path) {
    let width = report
        .tables
        .iter()
        .map(|table| table.table.len())
        .max()
        .unwrap_or(0);
    for table in &report.tables {
        println!("  {:<width$}  {:>8}", table.table, table.rows);
    }
    println!(
        "copied {} rows in {} tables in {:.2}s; point the coordinator at it with \
         {ENV_COORDINATOR_DB}={} and remove {ENV_COORDINATOR_DATABASE_URL}",
        report.total_rows(),
        report.tables.len(),
        report.elapsed.as_secs_f64(),
        target.display()
    );
}
