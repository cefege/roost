//! `roost add-machine` — mint one enrollment grant and print the command that
//! spends it, for a machine that is not this one. Called by the crate's
//! dispatcher. Depends on `status::service_definition` for what the installed
//! coordinator declares, on `roost-host` for every origin rule, on
//! `quickstart::grant` for the grant itself, and on `roost-platform` for the
//! command it prints — the browser's deploy dialog prints the same one.
//!
//! **The coordinator URL comes from the installed coordinator definition
//! first, and from this shell only when there is no definition to read.** Roost
//! installs no proxy, no tunnel and no certificate, so the only place the door
//! the fleet should dial is written down is the unit or plist an install
//! wrote. Reading the definition first is what stops a shell that happens to
//! export a different door from enrolling the next machine somewhere else.
//! Falling back to the shell is what lets an operator run this on a host whose
//! coordinator is not yet installed under a service manager, which is a real
//! case and a better answer than a refusal. Within each source the precedence
//! is `ROOST_COORDINATOR_URL`, then `ROOST_COORDINATOR_PUBLIC_URL`, then
//! `ROOST_WEB_PUBLIC_URL` — `GETTING_STARTED.md` records that order, and it is
//! the worker-dial precedence, not a browser one.
//!
//! **The printed command is the whole product.** stdout carries the command and
//! nothing else, so it can be pasted straight; every diagnostic, the minted
//! grant's lifetime, and the refusal text go to stderr.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Args;
use roost_host::coord_config_loader::{
    ENV_COORDINATOR_DATABASE_URL, ENV_COORDINATOR_DB, ENV_COORDINATOR_PUBLIC_URL,
    ENV_WEB_PUBLIC_URL,
};
use roost_host::{DatabaseLocation, EnvSource, HostPlatform, ProcessEnv, normalize_https_origin};
use roost_platform::machine_join_command;
use roost_worker::runtime::boot::ENV_COORDINATOR_URL;

use crate::command_error::CommandFailure;
use crate::quickstart::grant::{GrantKind, OneShotGrant, mint_host_grant};
use crate::status::service_definition::{
    InstalledEnvironment, declared_value, parse_installed_environment,
};
use crate::wall_clock;

/// The two dial variables the installed definition may declare, most specific
/// first. The first is an explicit worker target, the second is the door worker
/// traffic enters through, the third is the door the browser uses — and only
/// when nothing better is declared is the browser's door good enough to dial.
const DIAL_URL_NAMES: [&str; 3] = [
    ENV_COORDINATOR_URL,
    ENV_COORDINATOR_PUBLIC_URL,
    ENV_WEB_PUBLIC_URL,
];

/// What the operator says the new machine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentPlatform {
    /// macOS: a launchd agent.
    Macos,
    /// Linux: a systemd user unit.
    Linux,
}

impl EnrollmentPlatform {
    /// The names `--platform` accepts, and what each is called in the printed
    /// line.
    pub const NAMES: [(&'static str, Self); 2] = [("macos", Self::Macos), ("linux", Self::Linux)];

    /// Parse a `--platform` value, refusing anything else by name.
    ///
    /// This is clap's value parser rather than a derived enum on purpose. The
    /// refusal is the answer to "can I enroll a Windows machine?", and v2's
    /// usage line advertised `windows` on a product that has no Windows host
    /// install: an operator who typed it got a list of three and a token that
    /// could not be spent. Here the message says that v3 ships Linux and macOS
    /// only, and it is this function's text rather than a generated one.
    pub fn from_name(value: &str) -> Result<Self, String> {
        Self::NAMES
            .iter()
            .find(|(name, _)| *name == value)
            .map(|(_, platform)| *platform)
            .ok_or_else(|| {
                format!(
                    "--platform must be macos or linux, not {value:?}. Roost v3 has no Windows \
                     host install, so a Windows machine cannot be enrolled from here; a browser \
                     on Windows connects to the coordinator as a client."
                )
            })
    }

    /// What the printed line calls the machine, in the words an operator
    /// reading the command would use.
    pub const fn machine_label(self) -> &'static str {
        match self {
            EnrollmentPlatform::Macos => "Mac",
            EnrollmentPlatform::Linux => "Linux machine",
        }
    }
}

/// `roost add-machine --platform macos|linux [--label NAME]`.
#[derive(Debug, Args)]
#[command(
    about = "Mint a one-shot enrollment grant and print the command that spends it",
    long_about = "Mints a one-shot worker grant and prints a copy-pasteable enrollment command. \
                  Run it on the coordinator. The URL the new machine dials is read from this \
                  host's installed coordinator service definition; Roost derives none. \
                  Windows is not offered: v3 ships Linux and macOS only."
)]
pub struct AddMachineArgs {
    /// The operating system the new machine runs.
    #[arg(long, value_parser = EnrollmentPlatform::from_name, value_name = "macos|linux")]
    pub platform: EnrollmentPlatform,
    /// The name this machine appears under in the fleet. Unset, the coordinator
    /// names it from the key the new machine generates.
    #[arg(long, value_name = "NAME")]
    pub label: Option<String>,
}

pub async fn run(args: &AddMachineArgs) -> Result<ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let installed = installed_coordinator(&env, platform);
    let coordinator_url = dial_url(&installed, &env).ok_or_else(missing_url_refusal)?;
    let database = coordinator_database(&installed, &env).ok_or_else(missing_database_refusal)?;
    let label = args.label.clone().unwrap_or_default();
    if args.label.is_some() && label.chars().any(|character| character.is_control()) {
        return Err(CommandFailure::generic(
            "--label must be a single-line machine name with no control characters",
        ));
    }
    let grant = mint_host_grant(
        &database,
        GrantKind::Worker,
        grant_label(&label),
        wall_clock::now_ms(),
    )
    .await?;

    println!("Run this on the new {}:", args.platform.machine_label());
    println!();
    println!("{}", enrollment_command(&coordinator_url, &grant, &label));
    println!();
    eprintln!(
        "The grant is one-shot and is accepted for 24 hours. It is also written to the shell \
         history of the machine that runs it; on a shared account, run `add-machine` there."
    );
    eprintln!("Enrolled against {}", coordinator_url);
    Ok(ExitCode::SUCCESS)
}

/// The one line a new machine runs, over a live grant.
///
/// The only caller of the bearer in the whole crate. Everything else in this
/// module handles a grant as an opaque value.
pub fn enrollment_command(coordinator_url: &str, grant: &OneShotGrant, label: &str) -> String {
    machine_join_command(coordinator_url, grant.expose(), label)
}

/// The label recorded against the grant, which the coordinator shows while the
/// machine is still enrolling. An unnamed machine is still a distinguishable
/// one, so the fallback names the command that minted it rather than leaving
/// the row blank.
fn grant_label(label: &str) -> &str {
    if label.is_empty() {
        "add-machine"
    } else {
        label
    }
}

/// What the installed coordinator service declares, or nothing when there is no
/// install. A damaged definition is not an error here: `add-machine` is run by
/// hand on a machine that may have no coordinator at all, and the refusal below
/// is a better answer than a parse failure.
pub fn installed_coordinator(env: &dyn EnvSource, platform: HostPlatform) -> InstalledEnvironment {
    let Ok(definition_path) = roost_host::coord_service_path(env, platform) else {
        return InstalledEnvironment::new();
    };
    std::fs::read_to_string(definition_path)
        .map(|definition| parse_installed_environment(&definition, platform))
        .unwrap_or_default()
}

/// The door a new machine should dial, from the installed definition and then
/// from this shell.
///
/// The installed definition is tried first on purpose. A host that has run
/// `roost quickstart` with a front door has declared which door the fleet uses,
/// and a shell that happens to export a different one must not silently enroll
/// the next machine somewhere else.
pub fn dial_url(installed: &InstalledEnvironment, ambient: &dyn EnvSource) -> Option<String> {
    // Two whole passes, not one pass per name. A per-name pass answers "is
    // ENV_COORDINATOR_URL in the installed definition, and if not in the
    // shell?" — so a shell that exports the most specific name outranks an
    // installed definition that declares a different one, which is the exact
    // silent mis-enrollment this refuses. The installed definition is
    // therefore searched whole, in its own order, before the shell is read at
    // all.
    let declared = DIAL_URL_NAMES
        .iter()
        .find_map(|name| declared_value(installed, name).map(str::to_string))
        .or_else(|| DIAL_URL_NAMES.iter().find_map(|name| ambient.get(name)));
    declared.and_then(|declared| worker_dialable_origin(&declared).ok().flatten())
}

/// Normalize a declared door to something a worker on ANOTHER machine can dial.
///
/// A worker cannot reach this machine's loopback, and a door the coordinator
/// itself would refuse to boot with is not a door. Both are refused here rather
/// than printed into a command, because a command that cannot work is worse
/// than a refusal: the operator pastes it on a train.
pub fn worker_dialable_origin(declared: &str) -> Result<Option<String>, CommandFailure> {
    if declared.trim().is_empty() {
        return Ok(None);
    }
    let origin =
        normalize_https_origin(Some(declared.trim()), DIAL_URL_NAMES[0])?.ok_or_else(|| {
            CommandFailure::generic(format!(
                "{} is declared but is not a usable HTTPS origin",
                DIAL_URL_NAMES[0]
            ))
        })?;
    if is_loopback_host(&origin) {
        return Ok(None);
    }
    Ok(Some(origin))
}

/// Whether a URL's authority names this machine rather than the fleet. A
/// worker dialling it from another machine would reach nothing.
fn is_loopback_host(origin: &str) -> bool {
    let authority = origin
        .split("://")
        .nth(1)
        .unwrap_or(origin)
        .to_ascii_lowercase();
    authority == "localhost"
        || authority.ends_with(".localhost")
        || authority.starts_with("127.")
        || authority.starts_with("[::1]")
}

/// The coordinator's own database, as the installed definition names it, then
/// as this shell does.
///
/// Two whole passes for the reason `dial_url` gives: a shell that exports a
/// database must not outrank the installed definition's. Within a source the
/// Postgres URL wins, because the coordinator refuses to boot with both and a
/// definition carrying the URL is the one it actually boots with.
pub fn coordinator_database(
    installed: &InstalledEnvironment,
    ambient: &dyn EnvSource,
) -> Option<DatabaseLocation> {
    database_declared_by(|name| declared_value(installed, name).map(str::to_string))
        .or_else(|| database_declared_by(|name| ambient.get(name)))
}

/// The database one source declares, the Postgres URL first.
fn database_declared_by(lookup: impl Fn(&str) -> Option<String>) -> Option<DatabaseLocation> {
    let declared = |name: &str| lookup(name).filter(|value| !value.trim().is_empty());
    declared(ENV_COORDINATOR_DATABASE_URL)
        .map(DatabaseLocation::Postgres)
        .or_else(|| {
            declared(ENV_COORDINATOR_DB)
                .map(|path| DatabaseLocation::SqliteFile(PathBuf::from(path)))
        })
}

fn missing_url_refusal() -> CommandFailure {
    CommandFailure::generic(format!(
        "no coordinator URL is configured: set one of {} on this host's coordinator service, or \
         export it for this command. Roost derives none — a new machine has to dial the door the \
         operator actually put in front of the coordinator.",
        DIAL_URL_NAMES.join(", ")
    ))
}

pub(crate) fn missing_database_refusal() -> CommandFailure {
    CommandFailure::generic(format!(
        "this host's installed coordinator service declares neither \
         {ENV_COORDINATOR_DATABASE_URL} nor {ENV_COORDINATOR_DB}, and this shell exports neither, \
         so there is no database to record a grant in. Run `roost quickstart` on this machine \
         first, reinstall the coordinator service, or run this inside the coordinator's container."
    ))
}
