//! Which build a joining machine enrols as. Called by `quickstart::join`.
//! Depends on the deploy group's identity proof and on `roost-host`'s compiled
//! build identity.
//!
//! **A joined worker enrols as the build it is.** A binary carrying a compiled
//! commit (every release, and every build made inside a checkout) enrols as
//! that commit, because it is the stamp the installed worker reports in every
//! heartbeat and the fleet roster compares. Only a binary stamped `dev` has no
//! identity of its own; it proves a checkout instead, and that proof refuses a
//! dirty tree even where a deploy would accept the dirty stamp, because
//! enrollment is the moment a machine's identity is first asserted.

use std::path::{Path, PathBuf};

use roost_host::EnvSource;

use crate::command_error::CommandFailure;
use crate::deploy::codes;
use crate::deploy::identity::{ALLOW_DIRTY_ENV, DIRTY_SUFFIX, local_git_sha_or_die};
use crate::quickstart::join::declared;

/// The checkout a `dev`-stamped binary proves its build identity against. A
/// binary with a compiled commit enrols as that commit and never reads this;
/// for a build with no commit of its own, it names the tree the build came
/// from. Unset, the working directory is the tree.
pub const SOURCE_ROOT_ENV: &str = "ROOST_SOURCE_ROOT";

/// The checkout a join builds its identity from.
pub fn source_root(env: &dyn EnvSource) -> Result<PathBuf, CommandFailure> {
    if let Some(declared_root) = declared(env, SOURCE_ROOT_ENV) {
        return Ok(PathBuf::from(declared_root));
    }
    std::env::current_dir().map_err(|error| {
        CommandFailure::generic(format!(
            "the working directory could not be read, so there is no source tree to prove this \
             machine's build from; set {SOURCE_ROOT_ENV} to the checkout instead: {error}"
        ))
    })
}

/// The build this machine enrols as: the commit compiled into this binary, or,
/// for a `dev`-stamped binary, its checkout's commit through [`joined_build_sha`].
///
/// The compiled commit wins because it is what the installed worker reports. A
/// release fetched by `join.sh` runs from a staging directory that is no
/// checkout at all, and reading whatever checkout happens to be the working
/// directory would enrol the machine as some other build.
pub async fn join_identity(env: &dyn EnvSource) -> Result<String, CommandFailure> {
    match roost_host::COMPILED_ROOST_BUILD_SHA.filter(|sha| *sha != roost_host::DEV_BUILD_STAMP) {
        Some(compiled) => Ok(compiled.to_string()),
        None => joined_build_sha(&source_root(env)?).await,
    }
}

/// The build this machine's worker will be stamped with, refusing a dirty
/// checkout even when the operator allowed one elsewhere.
///
/// `ROOST_ALLOW_DIRTY=1` makes a deploy stamp `<sha>-dirty` and carry on. A
/// join may not: the stamp is the identity the fleet roster compares, and a
/// machine whose first assertion of identity is already wrong is a machine
/// whose drift badge is permanently wrong with no way to tell which build it
/// was.
pub async fn joined_build_sha(source_root: &Path) -> Result<String, CommandFailure> {
    let stamp = local_git_sha_or_die(source_root).await?;
    if stamp.ends_with(DIRTY_SUFFIX) {
        return Err(CommandFailure::new(
            codes::IDENTITY_UNPROVED,
            format!(
                "a joined worker requires a clean committed source snapshot, and {} has \
                 uncommitted changes. Commit them first. If you understand that the fleet will \
                 record this machine as {stamp} and can never afterwards tell which build it is \
                 running, set {ALLOW_DIRTY_ENV}=1 for this command alone.",
                source_root.display()
            ),
        ));
    }
    Ok(stamp)
}
