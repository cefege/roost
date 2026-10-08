//! The log rotation an install lays down beside a service definition: one
//! `logrotate.d` entry per role, and the pair of units that run the rotation,
//! because a *user* configuration is not read by the system timer.
//!
//! Called by the first-run and join install paths once a definition is on disk.
//! Depends on `roost-host`'s own log-directory, config-root and state-root
//! resolution and on this module's sibling renderers, which own the two log
//! file names and the unit format. It renders and reports the files; the caller
//! installs them, so that "a dry run writes nothing" stays a property of the
//! caller rather than of this module.
//!
//! **Why the timer exists at all.** A `logrotate.d` entry under the home
//! directory is read by nothing on its own: the system timer rotates
//! `/etc/logrotate.d`, and a service manager's `StandardOutput=append:` holds
//! the descriptor open across the rotate, so a rename-based rotation would leave
//! the service writing into an unlinked inode while the file on disk stopped
//! growing. `copytruncate` handles that; the user timer handles the fact that
//! nobody schedules it.
//!
//! **macOS gets nothing, and that is the v2 answer rather than a gap.** Both v2
//! shell installers branch on the platform before this step, so a macOS account
//! relies on `newsyslog`, which `roost logs` already tells the operator about.
//! Writing a Linux-only unit pair on a platform that has neither `logrotate`
//! nor a `systemd --user` manager would install files nothing ever reads.
//!
//! **Where the two roots come from.** `roost_host::config_root` and
//! `roost_host::state_root` own both rules, and this module calls them rather
//! than resolving `XDG_CONFIG_HOME` itself. It is worth knowing they were
//! unreachable here for a while: `xdg_root` was private and there was no config
//! constant at all, so the only way to place a `logrotate.d` fragment was to
//! restate the rule. The config root also has its own default for a reason that
//! is easy to get wrong — it is the one XDG root that is **not** under
//! `.local`.

use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, ProtocolResult, config_root, state_root};

use crate::services::definition_text::DEFINITION_MODE;
use crate::services::install::{InstallError, InstallOutcome, install_bytes};
use crate::services::service_spec::ServiceRole;
use crate::services::systemd_unit::{STDERR_FILE, STDOUT_FILE};

/// The directory logrotate is pointed at. One per install, and both roles'
/// entries live in it, because logrotate reads every file in the directory it
/// is given and two directories would need two units to cover them.
pub const CONF_DIR_NAME: &str = "logrotate.d";

/// The state subdirectory the rotation ledger lives in, below the XDG state
/// root.
const STATE_DIR_NAME: &str = "roost";

/// The rotation ledger logrotate reads to know what it already rotated in, and
/// the file an operator deletes to make it rotate everything again.
pub const STATUS_FILE_NAME: &str = "logrotate.status";

/// The oneshot unit that performs the rotation.
pub const SERVICE_FILE_NAME: &str = "roost3-logrotate.service";

/// The timer that triggers it.
pub const TIMER_FILE_NAME: &str = "roost3-logrotate.timer";

/// The rotation triggers on size rather than on age, so a chatty machine cannot
/// fill a disk between two daily runs.
const MAX_SIZE: &str = "size 100M";

/// How many compressed generations are kept. `roost doctor` reads the `.gz`
/// names, so the count is what bounds how far back a digest can see.
const RETAINED: &str = "rotate 5";

/// The program these files drive, which is also the file name looked for on
/// every search path entry.
const PROGRAM_NAME: &str = "logrotate";

/// Where a Linux distribution installs it when the account's search path does
/// not cover it, which is the ordinary case for a `--user` service.
const ABSOLUTE_FALLBACK: &str = "/usr/sbin/logrotate";

/// The `logrotate` program, resolved the way a shell installer resolves it:
/// `PATH` first, then the one absolute path a Linux box without it on `PATH`
/// installs it at. `None` means the machine has no `logrotate` at all, and the
/// caller reports that rather than writing a unit that cannot run.
pub fn resolve_rotate_binary(env: &dyn EnvSource) -> Option<PathBuf> {
    first_installed(candidates(env).into_iter())
}

/// The paths the program is looked for at, in the order they are tried.
fn candidates(env: &dyn EnvSource) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = env
        .get("PATH")
        .unwrap_or_default()
        .split(':')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| PathBuf::from(entry).join(PROGRAM_NAME))
        .collect();
    paths.push(PathBuf::from(ABSOLUTE_FALLBACK));
    paths
}

/// The first candidate that exists as a file, or `None` when none does.
///
/// Split out from the probe so the rule is decidable without a machine that
/// happens to have `logrotate` installed: the alternative is a test whose
/// answer depends on the box it runs on.
pub fn first_installed(mut candidates: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.find(|candidate| candidate.is_file())
}

/// One file an install writes, with the bytes it writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogrotateFile {
    /// Where the file goes.
    pub path: PathBuf,
    /// What goes in it.
    pub text: String,
}

/// Every file an install of `role` needs for its logs to be rotated.
///
/// Three files, not two: this role's `logrotate.d` entry, plus the rotation
/// unit and its timer. The unit pair is shared by both roles and rendered
/// identically from both, which is what makes installing them twice (once per
/// role) a byte-for-byte no-op the second time rather than a rewrite.
pub fn plan_files(
    env: &dyn EnvSource,
    platform: HostPlatform,
    role: ServiceRole,
    rotate_binary: &Path,
) -> ProtocolResult<Vec<LogrotateFile>> {
    let conf_dir = conf_dir(env)?;
    let unit_dir = role
        .definition_path(env, platform)?
        .parent()
        .map_or_else(|| conf_dir.clone(), Path::to_path_buf);
    Ok(vec![
        LogrotateFile {
            path: conf_dir.join(format!("{}.conf", role.service_label(env, platform)?)),
            text: conf_text(&role.log_dir(env, platform)?),
        },
        LogrotateFile {
            path: unit_dir.join(SERVICE_FILE_NAME),
            text: service_text(rotate_binary, &conf_dir, &status_path(env)?),
        },
        LogrotateFile {
            path: unit_dir.join(TIMER_FILE_NAME),
            text: TIMER_TEXT.to_string(),
        },
    ])
}

/// The `logrotate.d` entry for one role's two log files.
///
/// `copytruncate` rather than the default rename, because a service manager's
/// `StandardOutput=append:` holds the descriptor open: rotating by rename would
/// leave the running service appending to an inode with no name, and the file
/// the operator reads would stop growing without anything reporting an error.
fn conf_text(log_dir: &Path) -> String {
    format!(
        "{log_dir}/{STDOUT_FILE} {log_dir}/{STDERR_FILE} {{\n    {MAX_SIZE}\n    {RETAINED}\n    \
         compress\n    missingok\n    notifempty\n    copytruncate\n}}\n",
        log_dir = log_dir.display()
    )
}

/// The oneshot unit that runs the rotation over the whole conf directory.
fn service_text(rotate_binary: &Path, conf_dir: &Path, status: &Path) -> String {
    format!(
        "[Unit]\nDescription=Rotate Roost logs\n\n[Service]\nType=oneshot\nExecStart={} --state \
         {} {}\n",
        rotate_binary.display(),
        status.display(),
        conf_dir.display()
    )
}

/// The daily trigger. `Persistent` so a machine that was asleep at midnight
/// still rotates on the next boot instead of waiting a whole day.
const TIMER_TEXT: &str = "[Unit]\nDescription=Rotate Roost logs daily\n\n[Timer]\nOnCalendar=daily\n\
                          Persistent=true\n\n[Install]\nWantedBy=timers.target\n";

/// The directory holding this install's `logrotate` entries.
pub fn conf_dir(env: &dyn EnvSource) -> ProtocolResult<PathBuf> {
    Ok(config_root(env)?.join(CONF_DIR_NAME))
}

/// The file logrotate records what it has already rotated in.
pub fn status_path(env: &dyn EnvSource) -> ProtocolResult<PathBuf> {
    Ok(state_root(env)?.join(STATE_DIR_NAME).join(STATUS_FILE_NAME))
}

/// The rotation files for one role, or why there are none.
///
/// Skipping is an answer rather than a failure, and it is decided once, here,
/// so that a dry run and a real install cannot disagree about whether a machine
/// is getting a rotation. A macOS account has neither `logrotate` nor a
/// `systemd --user` manager, and v2's own installers install nothing there;
/// writing a Linux unit pair on that platform would put files on disk that
/// nothing ever reads, which is the same as a rotation that silently does not
/// happen while looking installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationPlan {
    /// The files, rendered and unwritten.
    Files(Vec<LogrotateFile>),
    /// There are none, and this is why.
    Skipped(&'static str),
}

/// Resolve what `role`'s rotation would be, writing nothing.
pub fn rotation_plan(
    env: &dyn EnvSource,
    platform: HostPlatform,
    role: ServiceRole,
) -> ProtocolResult<RotationPlan> {
    match platform {
        HostPlatform::Linux => {}
        HostPlatform::MacOs => return Ok(RotationPlan::Skipped(SKIP_MACOS)),
        HostPlatform::Windows => return Ok(RotationPlan::Skipped(SKIP_WINDOWS)),
    }
    let Some(rotate_binary) = resolve_rotate_binary(env) else {
        return Ok(RotationPlan::Skipped(SKIP_NO_BINARY));
    };
    Ok(RotationPlan::Files(plan_files(
        env,
        platform,
        role,
        &rotate_binary,
    )?))
}

/// What an install wrote, or why it deliberately wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationOutcome {
    /// The files were written; only the ones whose bytes changed are named.
    Installed(Vec<InstallOutcome>),
    /// Nothing was written, and this is why.
    Skipped(&'static str),
}

/// Install the rotation for `role`, and report what changed.
pub fn install_rotation(
    env: &dyn EnvSource,
    platform: HostPlatform,
    role: ServiceRole,
) -> Result<RotationOutcome, InstallError> {
    match rotation_plan(env, platform, role)? {
        RotationPlan::Skipped(reason) => Ok(RotationOutcome::Skipped(reason)),
        RotationPlan::Files(files) => {
            let mut installed = Vec::new();
            for file in files {
                installed.push(install_bytes(
                    &file.path,
                    file.text.as_bytes(),
                    DEFINITION_MODE,
                )?);
            }
            Ok(RotationOutcome::Installed(installed))
        }
    }
}

/// Why a macOS account gets no rotation files.
pub const SKIP_MACOS: &str =
    "macOS rotates these logs with newsyslog, so no logrotate files are installed";

/// Why a Windows account gets no rotation files.
pub const SKIP_WINDOWS: &str =
    "Windows has no logrotate; the launcher appends to main.out.log and main.err.log";

/// Why a Linux box with no `logrotate` gets none.
pub const SKIP_NO_BINARY: &str =
    "no logrotate program is installed here, so its logs will grow unbounded";
