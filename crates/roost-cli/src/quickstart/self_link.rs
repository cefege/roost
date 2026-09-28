//! `roost self-link` — putting this install's `roost` on the operator's `PATH`
//! as `~/.local/bin/roost`, and repairing that link when it is wrong. Called by
//! the crate's dispatcher and by the Phase 7 cutover, which runs it unattended
//! on a machine whose link may be absent, stale, or still pointing at an older
//! generation's install. Depends on `roost-host` for the release layout and on
//! `deploy::installed` for reading what an installed definition points at.
//!
//! **The link target is resolved, never guessed.** It is the `roost` inside the
//! release directory the INSTALLED service definition names, and only when
//! nothing is installed is it this build's own default program path. That is
//! the answer to "is this link still pointing at the old install", and it is an
//! equality check against a resolved path rather than a search for a version
//! string in a path: a v3 release directory and a v2 one are different
//! directories, and a command that decided which by pattern-matching `~/.roost`
//! would be guessing about a layout it does not own.
//!
//! **A link is repaired; a file is refused.** The entry under
//! `~/.local/bin/roost` is either a symlink this command owns, or it is
//! something an operator put there, and the two are not the same thing. A
//! broken link and a link to the wrong target are repaired — a link carries no
//! content, so replacing one destroys nothing. A regular file and a directory
//! are refused by name, with the exact command that clears them, because a
//! "repair" that overwrites a hand-written script is data loss at cutover.

use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, ProcessEnv};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::apply_release::ROOST_PROGRAM;
use crate::deploy::installed::installed_release_dir;
use crate::services::install::default_program_path;
use crate::services::service_environment::ENV_PATH;
use crate::services::service_spec::ServiceRole;

/// The directory, under the account's home, a shell looks in for a command the
/// operator installed themselves. The same directory the service definitions
/// put first on a service's own `PATH`, so `roost` resolves identically in a
/// login shell and inside a keeper.
const LOCAL_BIN: [&str; 2] = [".local", "bin"];

/// The program name under [`LOCAL_BIN`]. It is the same name the release ships
/// and `install-binary.sh` writes, so an operator who already installed by
/// hand and an operator who ran quickstart end up with the same command.
const LINK_NAME: &str = "roost";

/// What the command did, as a word an operator reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOutcome {
    /// The link was already correct; nothing was written.
    AlreadyCorrect,
    /// The link did not exist and was created.
    Created,
    /// The link existed and now points at this release.
    Repaired {
        /// What it pointed at before. `None` when it was a broken link, which
        /// reads as no target rather than as a path that is not there.
        previous: Option<PathBuf>,
    },
}

impl LinkOutcome {
    /// The sentence stdout carries, which is the answer to "what did it do?".
    pub fn sentence(&self, link: &Path, target: &Path) -> String {
        let arrow = format!("{} -> {}", link.display(), target.display());
        match self {
            LinkOutcome::AlreadyCorrect => format!("{arrow} (already correct)"),
            LinkOutcome::Created => arrow.clone(),
            LinkOutcome::Repaired { previous: None } => format!("{arrow} (replaced a broken link)"),
            LinkOutcome::Repaired {
                previous: Some(previous),
            } => format!("{arrow} (was {})", previous.display()),
        }
    }
}

/// Put `roost` on this account's `PATH`, repairing whatever is there.
pub fn run() -> Result<std::process::ExitCode, CommandFailure> {
    let env = ProcessEnv::new();
    let platform = roost_host::supported_host_platform()?;
    let home = env.home_dir().ok_or_else(|| {
        CommandFailure::generic(
            "self-link needs a home directory, and this process could not resolve one",
        )
    })?;
    let target = release_program(&env, platform)?;
    let bin_dir = home.join(Path::new(LOCAL_BIN[0]).join(LOCAL_BIN[1]));
    let link = bin_dir.join(LINK_NAME);

    let outcome = write_link(&link, &target)?;
    info!(link = %link.display(), target = %target.display(), "self-link settled");
    println!("{}", outcome.sentence(&link, &target));
    if !on_path(&env, &bin_dir) {
        eprintln!(
            "NOTE: {} is not on this shell's PATH; add it, or run {} directly.",
            bin_dir.display(),
            link.display()
        );
    }
    Ok(std::process::ExitCode::SUCCESS)
}

/// The `roost` this install's link must point at.
///
/// An installed definition is the authority because an operator who moved the
/// versions directory did it by editing the unit, and the unit is the only
/// place that survives. With nothing installed, this build's own default is the
/// answer — which is the same path a first install will write.
pub fn release_program(
    env: &dyn EnvSource,
    platform: HostPlatform,
) -> Result<PathBuf, CommandFailure> {
    for role in ServiceRole::ALL {
        let Ok(definition_path) = role.definition_path(env, platform) else {
            continue;
        };
        let Some(definition) = std::fs::read_to_string(&definition_path).ok() else {
            continue;
        };
        let Some(release) = installed_release_dir(&definition, platform) else {
            continue;
        };
        // `installed_release_dir` answers the directory the program sits in,
        // which both renderers agree on: systemd states it as the unit's
        // `WorkingDirectory=` and launchd as the program's parent.
        return Ok(release.join(ROOST_PROGRAM));
    }
    default_program_path(env, platform).map_err(Into::into)
}

/// Create or repair the link, and report which of the three it was.
///
/// Public so the repair can be proved against a throwaway home rather than the
/// operator's own `~/.local/bin`, which is the only way to assert that a
/// regular file is refused instead of being tested by an operator once.
pub fn write_link(link: &Path, target: &Path) -> Result<LinkOutcome, CommandFailure> {
    // Refused here rather than in the command body so the rule is enforced
    // where the link is made, and so a caller reaching this function by another
    // route gets the same answer. A dangling `roost` on PATH is worse than no
    // `roost`: every later command then fails in a way that looks like the
    // command itself is broken.
    if !target.is_file() {
        return Err(CommandFailure::generic(format!(
            "{} is not there, so there is no release for a link to point at. Run `roost \
             quickstart` to install one, or check the versions directory the installed service \
             definition names.",
            target.display()
        )));
    }
    let bin_dir = link.parent().ok_or_else(|| {
        CommandFailure::generic(format!(
            "{} names no directory to install into",
            link.display()
        ))
    })?;
    std::fs::create_dir_all(bin_dir).map_err(|error| {
        CommandFailure::generic(format!(
            "{} could not be created: {error}",
            bin_dir.display()
        ))
    })?;

    match std::fs::symlink_metadata(link) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::os::unix::fs::symlink(target, link).map_err(|error| {
                CommandFailure::generic(format!("{} could not be created: {error}", link.display()))
            })?;
            return Ok(LinkOutcome::Created);
        }
        Err(error) => {
            return Err(CommandFailure::generic(format!(
                "{} could not be inspected: {error}",
                link.display()
            )));
        }
        Ok(metadata) if !metadata.file_type().is_symlink() => {
            return Err(CommandFailure::generic(format!(
                "{} exists and is not a symlink, so this command will not overwrite it. \
                 Remove it with `rm {0}` and run `roost self-link` again.",
                link.display()
            )));
        }
        Ok(_) => {}
    }

    // `read_link` answers a DANGLING target as readily as a live one, so the
    // existence check is what makes a broken link report as `previous: None` —
    // "replaced a broken link" rather than "was <path>", which would name a
    // path the operator cannot look at.
    let previous = std::fs::read_link(link).ok().filter(|path| path.exists());
    if previous.as_deref() == Some(target) && target.is_file() {
        return Ok(LinkOutcome::AlreadyCorrect);
    }
    // The replacement link is built beside the entry it replaces and renamed
    // over it, so a cutover interrupted between the two leaves the previous
    // link intact rather than a half-written one.
    let staged = bin_dir.join(format!(".{LINK_NAME}.staged.{}", std::process::id()));
    let _ = std::fs::remove_file(&staged);
    std::os::unix::fs::symlink(target, &staged).map_err(|error| {
        CommandFailure::generic(format!("{} could not be staged: {error}", staged.display()))
    })?;
    if let Err(error) = std::fs::rename(&staged, link) {
        let _ = std::fs::remove_file(&staged);
        return Err(CommandFailure::generic(format!(
            "{} could not be replaced: {error}",
            link.display()
        )));
    }
    Ok(LinkOutcome::Repaired { previous })
}

/// Whether a directory is on this shell's `PATH`, so the command can say the
/// one thing that would still stop `roost` from working after a correct link.
fn on_path(env: &dyn EnvSource, bin_dir: &Path) -> bool {
    env.get(ENV_PATH)
        .is_some_and(|path| std::env::split_paths(&path).any(|entry| entry == bin_dir))
}
