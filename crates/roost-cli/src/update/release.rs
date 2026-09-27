//! Which release a machine would install, and getting its bytes onto this
//! machine's disk. Called by `update::mod`; depends on the update group's
//! candidate module and on `reqwest`, and on nothing else in this crate.
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

/// The GitHub repository every release is published from.
pub const RELEASE_REPOSITORY: &str = "cefege/roost";

/// The directory release assets are downloaded from, and this crate's ONLY read
/// of the mirror variable.
pub const RELEASE_BASE_URL_ENV: &str = "ROOST_RELEASE_BASE_URL";

/// The origin used when no mirror is configured.
pub const DEFAULT_RELEASE_BASE_URL: &str =
    "https://github.com/cefege/roost/releases/latest/download";

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

/// The directory release assets are downloaded from.
pub fn release_base_url(env: &dyn EnvSource) -> String {
    env.get(RELEASE_BASE_URL_ENV)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_RELEASE_BASE_URL.to_string())
}

/// Ask the origin which release is newest.
pub async fn fetch_latest_release_tag(
    env: &dyn EnvSource,
    arch: &str,
) -> Result<ReleaseListing, CommandFailure> {
    let client = reqwest::Client::builder()
        .timeout(LISTING_DEADLINE)
        .build()
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let response = client
        .get(format!(
            "https://api.github.com/repos/{RELEASE_REPOSITORY}/releases/latest"
        ))
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
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if tag.is_empty() {
        return Ok(ReleaseListing::none());
    }
    Ok(ReleaseListing {
        tag,
        arch: arch.to_string(),
    })
}

/// Download one release asset and prove it against the digest the release
/// published, staging it beside the executable it will replace.
///
/// The staging path is removed on every failure path, including a digest
/// mismatch, so a rejected candidate never survives the attempt. A candidate
/// left behind is a file a later confused step could pick up.
pub async fn download_and_verify(
    env: &dyn EnvSource,
    asset: &str,
    executable: &Path,
) -> Result<VerifiedCandidate, CandidateError> {
    let base = release_base_url(env);
    let url = format!("{}/{asset}", base.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(ASSET_DEADLINE)
        .build()
        .map_err(|error| CandidateError::DownloadFailed {
            asset: asset.to_string(),
            cause: error.to_string(),
        })?;
    let expected = fetch_published_digest(&client, &url, asset).await?;
    let (staged, file) = candidate::open_candidate(executable)?;
    let received = match stream_to_file(&client, &url, asset, file).await {
        Ok(digest) => digest,
        Err(failure) => {
            let _ = std::fs::remove_file(&staged);
            return Err(failure);
        }
    };
    if received != expected {
        let _ = std::fs::remove_file(&staged);
        return Err(CandidateError::DigestMismatch {
            asset: asset.to_string(),
            expected,
            actual: received,
        });
    }
    info!(asset, sha256 = %received, "release asset verified against its published digest");
    let verified = VerifiedCandidate {
        path: staged,
        sha256: received,
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
    let text = response.text().await.map_err(|error| {
        CandidateError::ChecksumUnreachableCause {
            asset: asset.clone(),
            cause: error.to_string(),
        }
    })?;
    candidate::parse_published_digest(&text)
        .ok_or(CandidateError::ChecksumMalformed { asset })
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
    let mut response = client
        .get(url.to_string())
        .send()
        .await
        .map_err(|error| CandidateError::DownloadFailed {
            asset: asset.clone(),
            cause: error.to_string(),
        })?;
    if !response.status().is_success() {
        return Err(CandidateError::DownloadUnreachable {
            asset,
            status: response.status().as_u16(),
        });
    }
    let mut hasher = sha2::Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| {
        CandidateError::DownloadFailed {
            asset: asset.clone(),
            cause: error.to_string(),
        }
    })? {
        hasher.update(&chunk);
        file.write_all(&chunk).map_err(|error| CandidateError::Unwritable {
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
