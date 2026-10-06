//! Where an install gets the web bundle from: a directory the operator names
//! with `--web-dist`, or the release the running binary came from. Called by
//! `quickstart`, `join` and `roost update`; depends on `update::release` for the
//! origin and the one verification every fetched asset goes through, and on the
//! services group's bundle installer.
//!
//! Every caller installing a RELEASE asks the same question: does this build's
//! own tag publish a bundle? That is what makes one function here rather than
//! one per command — a quickstart or join that installs a different bundle than
//! the update that follows it is a machine whose page changes under it.

use std::path::{Path, PathBuf};

use roost_host::EnvSource;

use crate::command_error::CommandFailure;
use crate::services::web_bundle;
use crate::update::release;

/// The tag whose published web bundle this build installs, or `None` for a
/// source build, which has no published tag to download one from.
pub fn published_bundle_tag(env: &dyn EnvSource) -> Option<String> {
    let version = roost_host::build_identity(env).artifact_version;
    (version != roost_host::DEV_BUILD_STAMP).then_some(version)
}

/// The web bundle a release install gets, downloaded from the release this
/// binary came from, or `None` when there is nothing to download it from.
///
/// A source build has no published tag, so it installs no bundle and says so
/// rather than refusing: enrollment is the one step a machine cannot do
/// without, and a missing page is a smaller problem than a machine that is not
/// in the fleet. A failed download IS a refusal, because silently installing
/// with no page would report success for a machine that serves 404s.
pub async fn install_web_bundle(
    env: &dyn EnvSource,
    bin_dir: &Path,
) -> Result<Option<PathBuf>, CommandFailure> {
    let Some(tag) = published_bundle_tag(env) else {
        eprintln!(
            ">> this is a source build, so there is no published web bundle to install; the \
             machine's door serves nothing until a release binary is installed"
        );
        return Ok(None);
    };
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
pub fn install_local_bundle(source: &Path, bin_dir: &Path) -> Result<PathBuf, CommandFailure> {
    let installed = web_bundle::install_from_dir(source, &web_bundle::release_web_dir(bin_dir))
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    eprintln!(
        ">> installed the web bundle ({} files) into {}",
        installed.files,
        installed.root.display()
    );
    Ok(installed.root)
}
