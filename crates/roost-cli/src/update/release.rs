//! Getting one release's bytes onto this machine's disk and proving them.
//! Called by `update::mod` and the deploy paths; depends on the update group's
//! candidate module and on `reqwest`, and on nothing else in this crate.
//!
//! The download names the tag it was given: a `latest/download` URL verifies
//! its own sidecar's digest perfectly while handing this binary a different
//! program. Which tag that is belongs to [`release_listing`](crate::update::release_listing).
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

pub use crate::update::assets::{WEB_ASSET_NAME, keeper_release_asset_name, release_asset_name};
use crate::update::candidate::{self, CandidateError, VerifiedCandidate};

/// The directory release assets are downloaded from, and this crate's ONLY read
/// of the mirror variable.
pub const RELEASE_BASE_URL_ENV: &str = "ROOST_RELEASE_BASE_URL";

/// Where a tag's assets live when no mirror is configured.
pub const RELEASE_DOWNLOAD_ORIGIN: &str = "https://github.com/cefege/roost/releases/download";

/// How long a checksum sidecar may take. It is a 65-byte file, so this is
/// patience rather than a transfer budget.
pub const CHECKSUM_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a release binary may take to arrive.
pub const ASSET_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

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
    let url = format!(
        "{}/{asset}{}",
        base.trim_end_matches('/'),
        candidate::SIDECAR_SUFFIX
    );
    let Ok(client) = reqwest::Client::builder()
        .timeout(CHECKSUM_DEADLINE)
        .build()
    else {
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
