//! The names a published release's assets are fetched under. Called by the
//! self-update and the deploy-fetch paths, which between them install the
//! `roost` binary, the keeper beside it and the web bundle; depends on nothing
//! in this crate but the shared `CommandFailure`.
//!
//! **One table, and the keeper's name is a substitution rather than a row.**
//! The release pipeline emits both programs from one matrix entry, so a name
//! present in one table and absent from the other is a 404 on one architecture
//! and not another — the shape of bug that ships. The web bundle is the one
//! asset whose name is not per-platform, and that is the shape the asset
//! actually has: one build of the page, four sets of binaries to serve it.

use roost_host::HostPlatform;

use crate::command_error::CommandFailure;

/// The release asset a platform/arch pair installs from.
///
/// `roost` stays unsuffixed for macOS arm64: it is byte-identical to
/// `roost-darwin-arm64` and exists so older release links keep resolving. The
/// installer mirrors this table, and a name present in one and absent from the
/// other is a 404 on one platform only — the shape of bug that ships.
pub fn release_asset_name(
    platform: HostPlatform,
    arch: &str,
) -> Result<&'static str, CommandFailure> {
    match (platform, normalized_arch(arch)?) {
        (HostPlatform::MacOs, "arm64") => Ok("roost"),
        (HostPlatform::MacOs, "x64") => Ok("roost-darwin-x64"),
        (HostPlatform::Linux, "x64") => Ok("roost-linux-x64"),
        (HostPlatform::Linux, "arm64") => Ok("roost-linux-arm64"),
        (unsupported, _) => Err(CommandFailure::generic(format!(
            "no published roost binary for {}",
            unsupported.display_name()
        ))),
    }
}

/// The keeper asset a platform/arch pair installs from.
///
/// The `roost` name with `roost` replaced by `roost-keeper`, never a second
/// table. The release pipeline emits both from one matrix entry, so a table
/// that could name a keeper asset the pipeline never publishes is a 404 on one
/// architecture and not another — the shape of bug that ships.
///
/// **A counted replacement, and the honest reason is defensiveness, not a case
/// that exists today.** `replacen` with a count of one is what this uses, and an
/// earlier version of this comment claimed the count was load-bearing because
/// `roost-darwin-x64` "names the program twice". It does not: the four
/// published names are `roost`, `roost-darwin-x64`, `roost-linux-x64` and
/// `roost-linux-arm64`, and `roost` occurs exactly once in each, so
/// `replace(ROOST_PROGRAM, …)` and `replacen(ROOST_PROGRAM, …, 1)` return the
/// same string for all of them. **The count is kept because a future suffixed
/// name could contain the program twice, and an uncounted replacement would
/// then rewrite the suffix as well** -- `roost-fallback-roost` becoming
/// `roost-keeper-fallback-roost-keeper`. That is a reason to keep the count, and
/// it is not the reason this comment used to give.
///
/// Returns an owned `String` where [`release_asset_name`] returns a `&'static
/// str`, because a substitution produces a new string rather than naming one.
/// Every caller builds a URL from it, so the allocation lands where the URL is
/// built and nowhere else.
pub fn keeper_release_asset_name(
    platform: HostPlatform,
    arch: &str,
) -> Result<String, CommandFailure> {
    Ok(release_asset_name(platform, arch)?.replacen(ROOST_PROGRAM, KEEPER_PROGRAM, 1))
}

/// The web bundle a release publishes, and the only asset whose name is not
/// per-platform.
///
/// One bundle serves all four targets: the page is the same build, differing
/// only in which binaries serve it. So one name is not a simplification here,
/// it is the shape the asset actually has, and a per-platform table would be
/// four chances to invent a name the pipeline does not emit.
pub const WEB_ASSET_NAME: &str = "roost-web.tar.gz";

/// The `roost` executable's own file name, and the prefix the keeper's name is
/// derived from. Both are what the release pipeline writes files under.
const ROOST_PROGRAM: &str = "roost";

/// The keeper executable beside it.
const KEEPER_PROGRAM: &str = "roost-keeper";

/// The architecture names a release pipeline and a Rust build disagree about,
/// folded to the release pipeline's spelling.
fn normalized_arch(arch: &str) -> Result<&'static str, CommandFailure> {
    match arch {
        "x86_64" | "amd64" | "x64" => Ok("x64"),
        "aarch64" | "arm64" => Ok("arm64"),
        other => Err(CommandFailure::generic(format!(
            "no published roost binary for architecture {other:?}"
        ))),
    }
}
