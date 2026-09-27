//! Fetching a published release's own binaries instead of building them here.
//! Called by the deploy command when `--release <tag>` is given; depends on
//! `update::release` for the origin, the asset names and the one verification
//! every fetched asset goes through, and on nothing else in the deploy group.
//!
//! **The point is reach.** A deploy that builds on the deploying box can only
//! ever reach machines whose target triple that box can produce: an x86_64
//! Linux coordinator cannot produce an aarch64 or a macOS binary, and three of
//! the production machines are exactly that. Fetching the target's own
//! published asset is what lets one coordinator reach the whole fleet, so the
//! staged tree this produces is deliberately the SAME tree `release::build_release`
//! produces — everything downstream of the staging is unchanged.
//!
//! **Published names and installed names are different things.** The release
//! publishes `roost-linux-x64` and `roost-keeper-linux-x64`; the staging tree,
//! `install_release` and `read_keeper_contract` all speak `roost` and
//! `roost-keeper`. Conflating them is a deploy that fetched two files and then
//! reported that the release ships no keeper.

use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::deploy::apply_release::RELEASE_BIN_DIR;
use crate::deploy::codes;
use crate::deploy::release::{StagedRelease, read_keeper_contract, release_digest};
use crate::services::web_bundle;
use crate::update::release;

/// The release a tag publishes, fetched rather than built, staged in the same
/// tree layout `build_release` produces.
///
/// **The point is reach, not speed.** A deploy that builds on the deploying box
/// can only ever reach machines whose target triple that box can produce: an
/// x86_64 Linux coordinator cannot build an aarch64 or a macOS binary, and
/// three of the production machines are exactly that. Fetching the target's own
/// published asset is what makes one coordinator able to reach the whole fleet.
///
/// The keeper contract is read from the DOWNLOADED `roost`, not from this
/// process, for the same reason `build_release` reads it from the staged
/// bytes: an admission decided against the bytes this CLI was built from is a
/// decision about the wrong program.
pub async fn fetch_release(
    env: &dyn EnvSource,
    tag: &str,
    platform: HostPlatform,
    arch: &str,
) -> Result<StagedRelease, CommandFailure> {
    let staging = std::env::temp_dir().join(format!("roost-release-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let bin_dir = staging.join(RELEASE_BIN_DIR);
    std::fs::create_dir_all(&bin_dir).map_err(|error| {
        codes::refuse(
            codes::BUILD_FAILED,
            format!(
                "cannot create the release tree at {}: {error}",
                staging.display()
            ),
        )
    })?;
    let staged = fetch_into(env, tag, platform, arch, &bin_dir).await;
    if let Err(failure) = staged {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(failure);
    }
    let keeper_contract = read_keeper_contract(&bin_dir.join(ROOST_PROGRAM))?;
    let web = staging.join(crate::services::web_bundle::WEB_DIR_NAME);
    let result = StagedRelease {
        digest: release_digest(&bin_dir)?,
        local_dir: bin_dir,
        git_sha: tag.to_string(),
        keeper_contract,
        web: web.is_dir().then_some(web),
    };
    Ok(result)
}

/// Fetch both programs, and the bundle when the release published one.
///
/// The bundle is optional HERE and not in `join`: a release predating the
/// bundle asset is a release that installs and runs, and refusing it would make
/// the first deployment of an older tag impossible. A release that publishes
/// the asset and has it 404 is a different case — `fetch_web` distinguishes the
/// two by whether the sidecar exists at all.
async fn fetch_into(
    env: &dyn EnvSource,
    tag: &str,
    platform: HostPlatform,
    arch: &str,
    bin_dir: &Path,
) -> Result<(), CommandFailure> {
    // The PUBLISHED name is per-platform and the INSTALLED name is not:
    // `roost-linux-x64` and `roost-keeper-linux-x64` are what the release
    // emits, and `roost` and `roost-keeper` are what the staging tree and
    // `install_release` both speak. Conflating them is a deploy that fetches
    // two files and then reports the release ships no keeper, because it is
    // looking for a name the release never publishes under.
    for (published, installed) in [
        (
            update::release::release_asset_name(platform, arch)?,
            ROOST_PROGRAM,
        ),
        (
            update::release::keeper_release_asset_name(platform, arch)?.as_str(),
            crate::deploy::apply_release::KEEPER_PROGRAM,
        ),
    ] {
        let file = std::fs::File::create(bin_dir.join(installed)).map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("cannot create the staged {installed}: {error}"),
            )
        })?;
        update::release::download_verified_to(env, tag, published, file)
            .await
            .map_err(|error| {
                codes::refuse(
                    codes::BUILD_FAILED,
                    format!("the {tag} release asset {published} could not be verified: {error}"),
                )
            })?;
        set_executable(&bin_dir.join(installed))?;
    }
    fetch_web(env, tag, bin_dir).await
}

/// The bundle, when the release published one beside its binaries.
async fn fetch_web(env: &dyn EnvSource, tag: &str, bin_dir: &Path) -> Result<(), CommandFailure> {
    let staging = bin_dir.parent().unwrap_or(bin_dir);
    let asset = update::release::WEB_ASSET_NAME;
    if !update::release::sidecar_is_published(env, tag, asset).await {
        info!(
            tag,
            "this release publishes no web bundle, so the target keeps serving whatever it has"
        );
        return Ok(());
    }
    let archive = staging.join(asset);
    let file = std::fs::File::create(&archive).map_err(|error| {
        codes::refuse(
            codes::BUILD_FAILED,
            format!("cannot create {}: {error}", archive.display()),
        )
    })?;
    update::release::download_verified_to(env, tag, asset, file)
        .await
        .map_err(|error| {
            codes::refuse(
                codes::BUILD_FAILED,
                format!("the {tag} web bundle could not be verified: {error}"),
            )
        })?;
    crate::services::web_bundle::install_from_tarball(
        &archive,
        &staging.join(crate::services::web_bundle::WEB_DIR_NAME),
    )
    .map_err(|error| codes::refuse(codes::BUILD_FAILED, error.to_string()))?;
    Ok(())
}

/// Make a fetched program executable, as a release's own tarball would ship it.
fn set_executable(path: &Path) -> Result<(), CommandFailure> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(|error| {
        codes::refuse(
            codes::BUILD_FAILED,
            format!("cannot make {} executable: {error}", path.display()),
        )
    })
}
