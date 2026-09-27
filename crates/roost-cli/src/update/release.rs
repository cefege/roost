//! Which release a machine would install, and getting its bytes onto this
//! machine's disk. Called by `update::mod`; depends on the update group's
//! candidate module and on `reqwest`, and on nothing else in this crate.
//!
//! **A v3 binary may only install a v3 release.** This repository publishes
//! both series from one tag namespace, so "the newest release" is a TypeScript
//! build today and a Rust build the day after. Resolution is therefore the
//! GitHub releases LISTING filtered to `v3.`, and the download names the tag it
//! chose: a `latest/download` URL verifies its own sidecar's digest perfectly
//! while handing this binary a different program.
//!
//! The origin is resolved once, here, from one variable. v2's self-updater
//! hardcoded the GitHub origin while the deploy paths honoured
//! `ROOST_RELEASE_BASE_URL`, so `roost update` could not be pointed at a mirror
//! at all. One resolver for both is the fix, and it is the fix rather than a
//! nicety: a release binary fetched from an origin the operator did not pin is a
//! binary the operator did not choose.
//!
//! The digest is checked BEFORE the body is fetched, not after. A 404 or a
//! tampered sidecar then costs a 65-byte request instead of a download, and a
//! body that fails its check leaves nothing on disk — a candidate that failed
//! verification sitting beside a live `roost` is exactly what a later confused
//! step would pick up.
use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform};
use tracing::info;

use crate::command_error::CommandFailure;
use crate::update::candidate::{self, CandidateError, VerifiedCandidate};

/// The GitHub repository every release is published from, named in both URLs
/// below rather than concatenated into them: a `const` cannot be built from
/// another, and two literals that have to agree are better as two literals
/// that a test can compare.
pub const RELEASE_REPOSITORY: &str = "cefege/roost";

/// The directory release assets are downloaded from, and this crate's ONLY read
/// of the mirror variable.
pub const RELEASE_BASE_URL_ENV: &str = "ROOST_RELEASE_BASE_URL";

/// The GitHub REST listing every published release, newest first. The `latest`
/// endpoint is deliberately not used: it answers with the newest release of
/// ANY series, and this repository's newest release today is a v2 binary.
pub const RELEASE_API_URL: &str = "https://api.github.com/repos/cefege/roost/releases?per_page=100";

/// Where a tag's assets live when no mirror is configured.
pub const RELEASE_DOWNLOAD_ORIGIN: &str = "https://github.com/cefege/roost/releases/download";

/// The tag prefix a release must carry to be installable by this binary.
///
/// THE WHOLE POINT OF THIS FILTER. `releases/latest` answers `v0.5.0`, a
/// TypeScript build, and a digest-verified download of it would replace a Rust
/// `roost` with a Bun one that answers none of the commands this contract
/// documents. Drafts are excluded because a draft has no assets anybody can
/// fetch, and pre-releases are INCLUDED because the fleet runs `v3.0.0-rc.N`
/// until `v3.0.0` exists.
pub const INSTALLABLE_TAG_PREFIX: &str = "v3.";

/// How long a checksum sidecar may take. It is a 65-byte file, so this is
/// patience rather than a transfer budget.
pub const CHECKSUM_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a release binary may take to arrive.
pub const ASSET_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// How long the release listing may take before "no release" is the answer.
pub const LISTING_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// The release this machine would install, and the architecture it is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseListing {
    /// The published tag, or empty when the origin named none.
    pub tag: String,
    /// The architecture the tag is for, which is what the asset is chosen for.
    pub arch: String,
}

impl ReleaseListing {
    /// The listing an unreachable or silent origin produces.
    ///
    /// No release and an unreachable origin are deliberately the SAME answer:
    /// both mean there is nothing this command can safely install, and telling
    /// an operator their update failed when the origin was merely down would
    /// send them to debug the wrong machine.
    pub fn none() -> Self {
        Self {
            tag: String::new(),
            arch: String::new(),
        }
    }
}

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
/// `replacen` with a count of ONE, and that detail is the whole reason this is
/// written as a substitution rather than as `replace`: `roost-darwin-x64`
/// names the program twice, and an un-counted replacement of the first
/// occurrence would ask for `roost-keeper-darwin-x64`, which is an asset name
/// the pipeline has never published.
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

/// The newest installable tag in a GitHub releases listing, or an empty tag
/// when the listing holds none.
///
/// Pure over the response body, so the choice this command makes is decidable
/// without a network. A v2 tag, a draft, and a body that is not a listing are
/// all "nothing to install", and a listing whose newest entry is `v0.5.0` with
/// `v3.0.0-rc.1` beneath it resolves to the rc — which is exactly what a
/// newest-of-any-series rule gets wrong.
pub fn newest_installable_tag(releases: &serde_json::Value) -> String {
    releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("draft").and_then(serde_json::Value::as_bool) != Some(true))
        .find_map(|entry| {
            let tag = entry
                .get("tag_name")
                .and_then(serde_json::Value::as_str)?
                .trim();
            tag.starts_with(INSTALLABLE_TAG_PREFIX)
                .then(|| tag.to_string())
        })
        .unwrap_or_default()
}

/// The directory one tag's assets are downloaded from, which is the mirror when
/// one is configured and the resolved tag's own directory when it is not.
///
/// A tag, never `latest`: `releases/latest/download` resolves to whatever
/// series published last, so the bytes a verified download fetched and the tag
/// this command records in its journal would be two different releases.
pub fn release_base_url(env: &dyn EnvSource, tag: &str) -> String {
    env.get(RELEASE_BASE_URL_ENV)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| format!("{RELEASE_DOWNLOAD_ORIGIN}/{tag}"))
}

/// Ask GitHub which installable release is newest.
///
/// No environment: the listing is GitHub's, and the one origin an operator may
/// substitute is the one the ASSETS come from, which [`release_base_url`]
/// resolves. A mirror that also republished the listing would be a second
/// release index to keep in step with the first, for no origin this command
/// cannot already be pointed at.
pub async fn fetch_latest_release_tag(arch: &str) -> Result<ReleaseListing, CommandFailure> {
    let client = reqwest::Client::builder()
        .timeout(LISTING_DEADLINE)
        .build()
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let response = client
        .get(RELEASE_API_URL)
        .header("accept", "application/vnd.github+json")
        .send()
        .await;
    let Ok(response) = response else {
        return Ok(ReleaseListing::none());
    };
    if !response.status().is_success() {
        return Ok(ReleaseListing::none());
    }
    let Ok(value) = response.json::<serde_json::Value>().await else {
        return Ok(ReleaseListing::none());
    };
    let tag = newest_installable_tag(&value);
    if tag.is_empty() {
        return Ok(ReleaseListing::none());
    }
    Ok(ReleaseListing {
        tag,
        arch: arch.to_string(),
    })
}

/// Download one release asset from one tag's directory and prove it against the
/// digest the release published, staging it beside the executable it replaces.
///
/// The tag is an argument, not something re-derived from the origin, because
/// the digest proves the BYTES and nothing about which release published them:
/// a `…/latest/download` URL fetches whatever series published last and passes
/// that sidecar's own check, which is how a v3 binary ends up replaced by a v2
/// one with every verification green.
///
/// The staging path is removed on every failure path, including a digest
/// mismatch, so a rejected candidate never survives the attempt. A candidate
/// left behind is a file a later confused step could pick up.
pub async fn download_and_verify(
    env: &dyn EnvSource,
    tag: &str,
    asset: &str,
    executable: &Path,
) -> Result<VerifiedCandidate, CandidateError> {
    let (staged, file) = candidate::open_candidate(executable)?;
    let (url, sha256) = match verified_download(env, tag, asset, file).await {
        Ok(verified) => verified,
        Err(failure) => {
            let _ = std::fs::remove_file(&staged);
            return Err(failure);
        }
    };
    let verified = VerifiedCandidate {
        path: staged,
        sha256,
        url,
    };
    // The bytes are read back from disk before anything may rename them, because
    // what gets installed is the file as it is NOW, not the body as it arrived.
    if let Err(failure) = candidate::confirm_staged_bytes(&verified) {
        let _ = std::fs::remove_file(&verified.path);
        return Err(failure);
    }
    Ok(verified)
}

/// Download one asset and prove it, writing the body into `file`.
///
/// The one verification this crate performs, for every asset it fetches from a
/// release: the published digest is read BEFORE the body is requested, and a
/// body that fails its check leaves `file` truncated rather than complete. The
/// binary, the keeper and the web bundle are three fetches, and three copies
/// of this would be three chances to ship one of them unverified — which is
/// the defect the previous single-asset shape could not have, and would have
/// grown the moment a second asset existed.
pub async fn download_verified_to(
    env: &dyn EnvSource,
    tag: &str,
    asset: &str,
    file: std::fs::File,
) -> Result<String, CandidateError> {
    verified_download(env, tag, asset, file)
        .await
        .map(|(_url, sha256)| sha256)
}

/// Fetch the sidecar, fetch the body, and refuse a body that does not match.
///
/// Returns the URL it fetched and the digest the bytes actually hashed to, so a
/// caller that stages beside an executable and a caller that stages into a
/// temporary file both record where the bytes came from.
async fn verified_download(
    env: &dyn EnvSource,
    tag: &str,
    asset: &str,
    file: std::fs::File,
) -> Result<(String, String), CandidateError> {
    let base = release_base_url(env, tag);
    let url = format!("{}/{asset}", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(ASSET_DEADLINE)
        .build()
        .map_err(|error| CandidateError::DownloadFailed {
            asset: asset.to_string(),
            cause: error.to_string(),
        })?;
    let expected = fetch_published_digest(&client, &url, asset).await?;
    let received = stream_to_file(&client, &url, asset, file).await?;
    if received != expected {
        return Err(CandidateError::DigestMismatch {
            asset: asset.to_string(),
            expected,
            actual: received,
        });
    }
    info!(asset, sha256 = %received, "release asset verified against its published digest");
    Ok((url, received))
}

/// Whether this release published a sidecar for `asset`.
///
/// Asked before an OPTIONAL asset is requested, so that a release predating one
/// asset is a release that installs rather than a 404 that aborts a deploy. The
/// sidecar is the probe rather than the body deliberately: it is one small
/// request, and a body that does not exist is a body worth never asking for.
pub async fn sidecar_is_published(env: &dyn EnvSource, tag: &str, asset: &str) -> bool {
    let base = release_base_url(env, tag);
    let url = format!("{}/{asset}{}", base.trim_end_matches('/'), candidate::SIDECAR_SUFFIX);
    let Ok(client) = reqwest::Client::builder().timeout(CHECKSUM_DEADLINE).build() else {
        return false;
    };
    client
        .get(url)
        .send()
        .await
        .is_ok_and(|response| response.status().is_success())
}

/// The digest the release published, refusing anything that is not one.
///
/// The parse is deliberately wide and that width is load-bearing:
/// `shasum -a 256` and `sha256sum` write different sidecar shapes and a mirror
/// that regenerates them with whatever tool it has must keep working. The digest
/// is compared byte for byte downstream either way, so narrowing this costs a
/// mirror and buys no security.
async fn fetch_published_digest(
    client: &reqwest::Client,
    url: &str,
    asset: &str,
) -> Result<String, CandidateError> {
    let asset = asset.to_string();
    let response = client
        .get(format!("{url}{}", candidate::SIDECAR_SUFFIX))
        .timeout(CHECKSUM_DEADLINE)
        .send()
        .await
        .map_err(|error| CandidateError::ChecksumUnreachableCause {
            asset: asset.clone(),
            cause: error.to_string(),
        })?;
    if !response.status().is_success() {
        return Err(CandidateError::ChecksumUnreachable {
            asset,
            status: response.status().as_u16(),
        });
    }
    let text = response
        .text()
        .await
        .map_err(|error| CandidateError::ChecksumUnreachableCause {
            asset: asset.clone(),
            cause: error.to_string(),
        })?;
    candidate::parse_published_digest(&text).ok_or(CandidateError::ChecksumMalformed { asset })
}

/// Stream a body to an open file, hashing as it goes so a release binary is
/// never held in memory to be checked.
async fn stream_to_file(
    client: &reqwest::Client,
    url: &str,
    asset: &str,
    mut file: std::fs::File,
) -> Result<String, CandidateError> {
    use sha2::Digest;
    use std::io::Write;

    let asset = asset.to_string();
    let mut response = client.get(url.to_string()).send().await.map_err(|error| {
        CandidateError::DownloadFailed {
            asset: asset.clone(),
            cause: error.to_string(),
        }
    })?;
    if !response.status().is_success() {
        return Err(CandidateError::DownloadUnreachable {
            asset,
            status: response.status().as_u16(),
        });
    }
    let mut hasher = sha2::Sha256::new();
    while let Some(chunk) =
        response
            .chunk()
            .await
            .map_err(|error| CandidateError::DownloadFailed {
                asset: asset.clone(),
                cause: error.to_string(),
            })?
    {
        hasher.update(&chunk);
        file.write_all(&chunk)
            .map_err(|error| CandidateError::Unwritable {
                path: PathBuf::from(url),
                cause: error.to_string(),
            })?;
    }
    file.flush().map_err(|error| CandidateError::Unwritable {
        path: PathBuf::from(url),
        cause: error.to_string(),
    })?;
    Ok(hex::encode(hasher.finalize()))
}

/// The architecture this host reports, for choosing the release asset.
pub fn host_arch(platform: HostPlatform) -> &'static str {
    match (platform, std::env::consts::ARCH) {
        (_, "aarch64") => "aarch64",
        (_, "x86_64") => "x86_64",
        (_, _) => "unknown",
    }
}
