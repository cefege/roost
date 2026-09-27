//! Where an install gets the web bundle from: a directory an operator names, or
//! the release the running binary came from. Called by the three install paths;
//! take a directory on the command line. Called by `join` and by
//! `roost update`; depends on `update::release` for the origin and the one
//! verification every fetched asset goes through, and on this group's own
//! bundle installer.
//!
//! Both callers are installing a RELEASE, so both ask the same question: does
//! this build's own tag publish a bundle? That is what makes one function here
//! rather than one per command — a join that installs a different bundle than
//! the update that follows it is a machine whose page changes under it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use roost_host::EnvSource;
use roost_protocol::ProtocolError;

use crate::command_error::CommandFailure;
use crate::services::web_bundle;
use crate::update::release;

/// The web bundle a joined machine gets, downloaded from the release this
/// binary came from, or `None` when there is nothing to download it from.
///
/// A source build has no published tag, so it installs no bundle and says so
/// rather than refusing to join: enrollment is the one step a machine cannot do
/// without, and a missing page is a smaller problem than a machine that is not
/// in the fleet. A failed download IS a refusal, because silently joining with
/// no page would report success for a machine that serves 404s.
pub async fn install_web_bundle(
    env: &dyn EnvSource,
    bin_dir: &Path,
) -> Result<Option<PathBuf>, CommandFailure> {
    let identity = roost_host::build_identity(env);
    if identity.artifact_version == roost_host::DEV_BUILD_STAMP {
        eprintln!(
            ">> this is a source build, so there is no published web bundle to install; the \
             machine's door serves nothing until a release binary is installed"
        );
        return Ok(None);
    }
    let tag = identity.artifact_version.clone();
    let archive = download_web_bundle(env, &tag).await?;
    let destination = web_bundle::release_web_dir(bin_dir);
    match web_bundle::install_from_tarball(&archive, &destination) {
        Ok(installed) => {
            let _ = std::fs::remove_file(&archive);
            eprintln!(
                ">> installed the web bundle ({} files) into {}",
                installed.files,
                installed.root.display()
            );
            Ok(Some(installed.root))
        }
        Err(error) => {
            let _ = std::fs::remove_file(&archive);
            Err(CommandFailure::generic(error.to_string()))
        }
    }
}

/// Fetch the release's `roost-web.tar.gz` and prove it against the digest the
/// same release published for it.
pub async fn download_web_bundle(
    env: &dyn EnvSource,
    tag: &str,
) -> Result<PathBuf, CommandFailure> {
    let path = std::env::temp_dir().join(format!("roost-web-{tag}.tar.gz"));
    let file = std::fs::File::create(&path).map_err(|error| {
        CommandFailure::generic(format!(
            "cannot create {} to download the web bundle into: {error}",
            path.display()
        ))
    })?;
    match release::download_verified_to(env, tag, release::WEB_ASSET_NAME, file).await {
        Ok(_) => Ok(path),
        Err(error) => {
            let _ = std::fs::remove_file(&path);
            Err(CommandFailure::generic(format!(
                "the web bundle published with {tag} could not be fetched and verified: {error}"
            )))
        }
    }
}

/// Install a `--web-dist` beside the release's executables, and report the
/// directory both definitions will be pointed at.
pub fn install_local_bundle(
    web_dist: Option<&Path>,
    bin_dir: &Path,
) -> Result<Option<PathBuf>, CommandFailure> {
    let Some(source) = web_dist else {
        return Ok(None);
    };
    let installed = web_bundle::install_from_dir(source, &web_bundle::release_web_dir(bin_dir))
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    eprintln!(
        ">> installed the web bundle ({} files) into {}",
        installed.files,
        installed.root.display()
    );
    Ok(Some(installed.root))
}
