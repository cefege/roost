//! `roost db-to-postgres` — copy this install's SQLite coordinator database
//! (or any coordinator SQLite file) into a Postgres database, so the
//! coordinator can run stateless against it. Called by the crate dispatcher;
//! the copy itself is `roost_coord::db::sqlite_to_postgres`, which owns the
//! schema both ends are migrated to.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use roost_coord::db::sqlite_to_postgres::{
    ExistingRows, TransferError, TransferReport, copy_sqlite_to_postgres,
};
use roost_host::coord_config_loader::{ENV_COORDINATOR_DATABASE_URL, ENV_COORDINATOR_DB};
use roost_host::{DatabaseLocation, EnvSource, HostPlatform, ProcessEnv, coord_service_label};

use crate::command_error::CommandFailure;
use crate::import_v2::{CoordinatorState, coordinator_database, coordinator_state};
use crate::quickstart::add_machine::installed_coordinator;
use crate::status::service_definition::declared_value;

/// `roost db-to-postgres`.
#[derive(Debug, Args)]
#[command(about = "Copy a SQLite coordinator database into Postgres")]
pub struct DbToPostgresArgs {
    /// The SQLite file to copy. Default: the file this host's installed
    /// coordinator declares, else the default data directory's.
    #[arg(long, value_name = "PATH")]
    pub from: Option<PathBuf>,
    /// The target `postgres://` URL. Default: `ROOST_COORDINATOR_DATABASE_URL`
    /// from this shell, which keeps the password out of the process list.
    #[arg(long, value_name = "URL")]
    pub to: Option<String>,
    /// Empty every coordinator table in the target before copying, instead of
    /// refusing a target that already holds rows.
    #[arg(long)]
    pub replace: bool,
}

/// Run the command.
pub async fn run(args: &DbToPostgresArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let installed_source = installed_sqlite_file(&env, platform);
    let source = match &args.from {
        Some(path) => path.clone(),
        None => match &installed_source {
            Some(path) => path.clone(),
            None => coordinator_database(&env, platform)?,
        },
    };
    let target = target_url(args.to.as_deref(), &env)?;
    refuse_if_source_is_live(&env, platform, &source, installed_source.as_deref())?;

    let existing = if args.replace {
        ExistingRows::Replace
    } else {
        ExistingRows::Refuse
    };
    println!("copying {} into Postgres…", source.display());
    let report = copy_sqlite_to_postgres(&source, &target, existing)
        .await
        .map_err(|error| transfer_failure(&error))?;
    print_report(&report);
    Ok(ExitCode::SUCCESS)
}

/// The SQLite file this host's installed coordinator boots with, if it
/// declares one, else the one this shell names.
fn installed_sqlite_file(env: &dyn EnvSource, platform: HostPlatform) -> Option<PathBuf> {
    let installed = installed_coordinator(env, platform);
    declared_value(&installed, ENV_COORDINATOR_DB)
        .map(str::to_owned)
        .or_else(|| env.get(ENV_COORDINATOR_DB))
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

/// The target URL, from the flag or the shell, and refused unless it names
/// Postgres.
pub fn target_url(flag: Option<&str>, env: &dyn EnvSource) -> Result<String, CommandFailure> {
    let url = flag
        .map(str::to_owned)
        .or_else(|| env.get(ENV_COORDINATOR_DATABASE_URL))
        .filter(|url| !url.trim().is_empty())
        .ok_or_else(|| {
            CommandFailure::usage(format!(
                "no target database: pass --to postgres://… or export \
                 {ENV_COORDINATOR_DATABASE_URL}"
            ))
        })?;
    if !DatabaseLocation::is_postgres_url(&url) {
        return Err(CommandFailure::usage(
            "the target must be a postgres:// or postgresql:// URL",
        ));
    }
    Ok(url)
}

/// A running coordinator writing the source mid-copy would leave the target
/// missing those writes, with nothing to say so.
fn refuse_if_source_is_live(
    env: &dyn EnvSource,
    platform: HostPlatform,
    source: &Path,
    installed_source: Option<&Path>,
) -> Result<(), CommandFailure> {
    let Some(installed_source) = installed_source else {
        return Ok(());
    };
    if !same_file(source, installed_source) {
        return Ok(());
    }
    let label = coord_service_label(env, platform)?;
    match coordinator_state(platform, &label) {
        CoordinatorState::Running => Err(CommandFailure::usage(format!(
            "the coordinator ({label}) is running against {}, so rows written during the copy \
             would be lost. Stop it first (`systemctl --user stop {label}` or `launchctl bootout`), \
             then run this again.",
            source.display()
        ))),
        CoordinatorState::Stopped | CoordinatorState::Unavailable => Ok(()),
    }
}

fn same_file(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// A refusal the operator can fix is exit 2; anything else is a failure.
fn transfer_failure(error: &TransferError) -> CommandFailure {
    match error {
        TransferError::SourceMissing(_) | TransferError::TargetNotEmpty { .. } => {
            let hint = if matches!(error, TransferError::TargetNotEmpty { .. }) {
                " Pass --replace to empty it first; stop any coordinator using it."
            } else {
                ""
            };
            CommandFailure::usage(format!("{error}.{hint}"))
        }
        _ => CommandFailure::generic(format!("nothing was written to Postgres: {error}")),
    }
}

fn print_report(report: &TransferReport) {
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
         {ENV_COORDINATOR_DATABASE_URL} and remove {ENV_COORDINATOR_DB}",
        report.total_rows(),
        report.tables.len(),
        report.elapsed.as_secs_f64()
    );
}

#[cfg(test)]
mod tests {
    use super::target_url;
    use roost_host::MapEnv;
    use roost_host::coord_config_loader::ENV_COORDINATOR_DATABASE_URL;

    #[test]
    fn the_flag_outranks_the_shell_and_a_non_postgres_url_is_refused() {
        let env = MapEnv::new().with(ENV_COORDINATOR_DATABASE_URL, "postgres://shell/db");
        assert_eq!(
            target_url(Some("postgresql://flag/db"), &env).expect("a postgres flag"),
            "postgresql://flag/db"
        );
        assert_eq!(
            target_url(None, &env).expect("the shell's url"),
            "postgres://shell/db"
        );
        let refused = target_url(Some("sqlite:///tmp/x.db"), &env).expect_err("not postgres");
        assert_eq!(refused.code, crate::command_error::REJECTED_INVOCATION);
        let missing = target_url(None, &MapEnv::new()).expect_err("nothing named");
        assert_eq!(missing.code, crate::command_error::REJECTED_INVOCATION);
    }
}
