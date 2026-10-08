//! `roost import-v2` — carry a v2 coordinator's identity across the cutover,
//! once, read-only. Called by the crate's dispatcher; depends on
//! `roost-coord` for the target database and its single-tenant invariant, on
//! `roost-host` for the paths the coordinator itself resolves, and on the
//! `plan` and `copy` siblings for what is copied and how.
//!
//! **WHY THIS EXISTS, AND WHY IT IS ONCE.** Browser keys are origin-bound: a
//! paired browser cannot be handed to another origin and expected to work, so
//! the only way a browser survives the cutover is for its key to already be in
//! the v3 coordinator's database. This command puts it there, and it is the
//! only code in the tree that opens a v2 database — the COORDINATOR never
//! does, which is the invariant that keeps a v3 install from growing a
//! migration path it would have to support forever.
//!
//! **IT MUST RUN BEFORE ANYTHING CREATES THE v3 DATABASE.**
//! `ensure_self_hosted_tenant` creates a fresh account in an empty database,
//! and a later import could not reconcile that: the account it carried across
//! would be a second one, and the coordinator refuses two. The order is
//! therefore part of the command, and the refusal below enforces the half of
//! it an operator can get wrong — a coordinator already running against the
//! database this command is about to rewrite.

pub mod copy;
pub mod plan;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Args;
use roost_coord::auth::self_hosted_tenant;
use roost_host::{EnvSource, HostPlatform, ProcessEnv, coord_data_dir, coord_service_label};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::import_v2::copy::TableReport;
use crate::import_v2::plan::ImportMode;
use crate::services::scheduled_task::{powershell_argv, running_probe_script};
use crate::status::service_probe::current_uid;
use crate::wall_clock;

/// `roost import-v2` — the one command that reads another product's database.
#[derive(Debug, Args)]
#[command(about = "Carry a v2 coordinator's account, devices and keys into this install")]
pub struct ImportV2Args {
    /// The v2 coordinator database to read. Opened read-only and never written.
    #[arg(long, value_name = "PATH")]
    pub from: PathBuf,
    /// Report what an import would do, and write nothing at all.
    #[arg(long)]
    pub dry_run: bool,
}

/// What the service manager said about this install's coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorState {
    /// The unit is loaded and running. The database is being written to.
    Running,
    /// The unit is not running: stopped, not installed, or never loaded.
    Stopped,
    /// The service manager could not be asked, which is not the same as stopped.
    Unavailable,
}

/// The argv that asks the platform's service manager whether the coordinator
/// is running.
///
/// argv rather than a shell string for the reason every other service call in
/// this crate is argv: a unit name can come from the environment, and a shell
/// string is a quoting bug waiting for a name with a space in it.
#[must_use]
pub fn coordinator_probe_argv(platform: HostPlatform, label: &str) -> Vec<String> {
    match platform {
        HostPlatform::Linux => vec![
            "systemctl".to_string(),
            "--user".to_string(),
            "is-active".to_string(),
            label.to_string(),
        ],
        HostPlatform::MacOs => vec![
            "launchctl".to_string(),
            "print".to_string(),
            format!("gui/{}/{}", current_uid(), label),
        ],
        HostPlatform::Windows => powershell_argv(&running_probe_script(label)),
    }
}

/// Read the service manager's answer, given a probe that runs one argv.
pub fn coordinator_state_with(
    platform: HostPlatform,
    label: &str,
    mut probe: impl FnMut(&[String]) -> Option<String>,
) -> CoordinatorState {
    let argv = coordinator_probe_argv(platform, label);
    if argv.is_empty() {
        return CoordinatorState::Unavailable;
    }
    let Some(answer) = probe(&argv) else {
        return CoordinatorState::Unavailable;
    };
    // systemd's `is-active` exits non-zero and prints `inactive` for a unit
    // that is not running, and exits non-zero with no useful word for a unit
    // it has never heard of. Only an explicit `active` is a running
    // coordinator; everything else is a database nobody is writing to.
    if answer.trim() == "active" {
        CoordinatorState::Running
    } else {
        CoordinatorState::Stopped
    }
}

/// Ask the real service manager.
pub fn coordinator_state(platform: HostPlatform, label: &str) -> CoordinatorState {
    coordinator_state_with(platform, label, |argv| {
        let output = std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .ok()?;
        if !output.status.success() {
            // A manager that reported failure has not said `active`, and the
            // one case that matters — a unit it does not know — is a failure.
            return Some(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        Some(String::from_utf8_lossy(&output.stdout).into_owned())
    })
}

/// Run the command.
pub async fn run(args: &ImportV2Args) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    if !args.from.is_file() {
        return Err(CommandFailure::usage(format!(
            "{} is not a file. --from names the v2 coordinator database, \
             ~/.local/share/RoostCoordinatorV2/coordinator_v2.db on a Linux install.",
            args.from.display()
        )));
    }
    refuse_if_coordinator_running(&env, platform)?;

    let target = coordinator_database(&env, platform)?;
    if args.dry_run {
        let reports = copy::estimate(&args.from, &target).await?;
        return report(&reports, None, true);
    }
    let applied = apply(&args.from, &target, wall_clock::now_ms()).await?;
    report(&applied.reports, Some(applied.mode), false)
}

/// Where this install's coordinator keeps its database, resolved exactly the
/// way the coordinator resolves it, so an import cannot land in a file the
/// coordinator will not read.
pub fn coordinator_database(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<PathBuf, CommandFailure> {
    Ok(coord_data_dir(env, platform)?.join(roost_host::coord_config::COORD_DB_FILE_NAME))
}

/// A running coordinator holds the database this command is about to rewrite.
fn refuse_if_coordinator_running(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<(), CommandFailure> {
    let label = coord_service_label(env, platform)?;
    match coordinator_state(platform, &label) {
        CoordinatorState::Running => Err(CommandFailure::usage(format!(
            "the coordinator ({label}) is running, so its database is open and this import would \
             rewrite it underneath a live process. Stop it, run this, and start it again: \
             `systemctl --user stop {label}`, `roost import-v2 --from …`, \
             `systemctl --user start {label}`."
        ))),
        CoordinatorState::Stopped | CoordinatorState::Unavailable => Ok(()),
    }
}

/// One import, from the source file to a validated target.
#[derive(Debug)]
pub struct Applied {
    /// How the target answered.
    pub mode: ImportMode,
    /// What each table contributed.
    pub reports: Vec<TableReport>,
}

/// Open, copy, and prove. Every step here is ordered by what depends on what.
pub async fn apply(source: &Path, target: &Path, now_ms: i64) -> Result<Applied, CommandFailure> {
    let source_pool = copy::open_source_read_only(source).await?;
    let account = copy::source_account(&source_pool).await?;
    source_pool.close().await;

    // The coordinator's own open, so v3's migrations run and the file is
    // exactly the one the coordinator will use. A target that does not exist
    // yet is created here and only here: an import that skipped this would
    // write rows into a schema nothing has migrated. Its directory too: the
    // import runs BEFORE quickstart, so on a host that has never had v3 nothing
    // has created the data directory, and SQLite creates a file, not a path.
    if let Some(directory) = target.parent() {
        std::fs::create_dir_all(directory).map_err(|error| {
            CommandFailure::generic(format!(
                "the v3 data directory {} could not be created: {error}",
                directory.display()
            ))
        })?;
    }
    let target_location = roost_host::DatabaseLocation::SqliteFile(target.to_path_buf());
    let database = roost_coord::db::open(&target_location)
        .await
        .map_err(|error| {
            CommandFailure::generic(format!(
                "the v3 database {} could not be opened: {error}",
                target.display()
            ))
        })?;
    copy::attach(database.pool(), source).await?;
    let (mode, reports) = copy::apply(database.pool(), &account).await?;

    // The validator. `ensure_self_hosted_tenant` is the coordinator's own
    // single-tenant rule, not a second copy of it: it refuses a topology this
    // command would otherwise have written happily, and the account it hands
    // back must be the account the import carried, or the import landed in a
    // database whose identity is somebody else's.
    let tenant = self_hosted_tenant::ensure_self_hosted_tenant(&database, now_ms)
        .await
        .map_err(|error| {
            CommandFailure::generic(format!(
                "the imported database is not a valid self-hosted install: {error}"
            ))
        })?;
    if tenant.account_id != account {
        return Err(CommandFailure::generic(format!(
            "the imported identity is {} but this install resolved to {}, so the import did not \
             land in the account it was carrying across",
            account, tenant.account_id
        )));
    }
    info!(
        account = %tenant.account_id,
        tables = reports.len(),
        "roost import-v2 carried a v2 identity across",
    );
    Ok(Applied { mode, reports })
}

/// Print the answer, and say plainly that nothing was written when nothing was.
fn report(
    reports: &[TableReport],
    mode: Option<ImportMode>,
    dry_run: bool,
) -> Result<ExitCode, CommandFailure> {
    if dry_run {
        println!("nothing below was written; this is what a real run would do");
    } else {
        match mode {
            Some(ImportMode::FirstRun) => {
                println!("first import: this v3 install had no account, so everything was written");
            }
            Some(ImportMode::Refresh) => {
                println!(
                    "re-import: this v3 install was already set up, so nothing was overwritten"
                );
            }
            None => {}
        }
    }
    for line in reports {
        println!("{}", line.line());
    }
    Ok(ExitCode::SUCCESS)
}
