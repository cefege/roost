//! Release listing and branch-aware selection for `roost update`.
//! GitHub version order selects candidates; compare admits their ancestry.
//! An unreadable compare preserves listing-based selection with a warning,
//! so rate limiting does not disable every update.
use tracing::{info, warn};

use crate::command_error::CommandFailure;
use crate::update::version::{ReleaseChannel, ReleaseVersion};

/// The GitHub REST listing every published release, newest first. The `latest`
/// endpoint is deliberately not used: it answers with the newest release of
/// ANY series, and this repository's newest release today is a v2 binary.
pub const RELEASE_API_URL: &str = "https://api.github.com/repos/cefege/roost/releases?per_page=100";

/// The endpoint that answers whether one ref contains another. Asked as
/// `compare/v3...<tag>`, it answers the one question a tag name cannot: is this
/// release's commit on the branch releases are cut from.
pub const RELEASE_COMPARE_URL: &str = "https://api.github.com/repos/cefege/roost/compare";

/// The `User-Agent` every request to `api.github.com` must carry. GitHub
/// refuses a request without one with `403`, which reads exactly like an empty
/// listing, so its absence disabled `roost update` silently.
pub const GITHUB_USER_AGENT: &str = concat!("roost/", env!("CARGO_PKG_VERSION"));

/// The tag prefix a release must carry to be installable by this binary.
///
/// THE WHOLE POINT OF THIS FILTER. `releases/latest` answers `v0.5.0`, a
/// TypeScript build, and a digest-verified download of it would replace a Rust
/// `roost` with a Bun one that answers none of the commands this contract
/// documents. Drafts are excluded because a draft has no assets anybody can
/// fetch; pre-releases are candidates only on the pre-release channel
/// (`update::version::release_channel`).
pub const INSTALLABLE_TAG_PREFIX: &str = "v3.";

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

/// Every installable tag on `channel`, highest version first.
///
/// The caller walks this list rather than taking its head, because version
/// order says nothing about where a tag was cut.
pub fn installable_tags(
    releases: &serde_json::Value,
    channel: ReleaseChannel,
) -> impl Iterator<Item = &str> {
    let mut candidates: Vec<(ReleaseVersion, &str)> = releases
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| entry.get("draft").and_then(serde_json::Value::as_bool) != Some(true))
        .filter_map(|entry| {
            let tag = entry
                .get("tag_name")
                .and_then(serde_json::Value::as_str)?
                .trim();
            if !tag.starts_with(INSTALLABLE_TAG_PREFIX) {
                return None;
            }
            let version = ReleaseVersion::parse(tag).ok()?;
            version.is_on(channel).then_some((version, tag))
        })
        .collect();
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    candidates.into_iter().rev().map(|(_, tag)| tag)
}

/// What a compare answer says about a candidate tag: `Some(true)` for a tag
/// whose commit is on the branch, `Some(false)` for one carrying work that is
/// not, and `None` for an answer that says neither.
///
/// GitHub's statuses are `identical`, `ahead`, `behind` and `diverged`. A tag
/// cut from `v3` is `identical` to it or `behind` it, which includes every
/// release of a fleet that rolled back. `ahead` and `diverged` describe a
/// commit `v3` does not contain.
pub fn compare_status_allows(status: &str) -> Option<bool> {
    match status {
        "identical" | "behind" => Some(true),
        "ahead" | "diverged" => Some(false),
        _ => None,
    }
}

/// Ask GitHub which installable release on `channel` is highest.
///
/// No environment: the listing is GitHub's, and the one origin an operator may
/// substitute is the one the ASSETS come from, which
/// [`release_base_url`](crate::update::release::release_base_url) resolves. A
/// mirror that also republished the listing would be a second release index to
/// keep in step with the first, for no origin this command cannot already be
/// pointed at.
///
/// Candidates are tried highest-version first and each is checked with
/// `tag_is_on_v3`, so a `v3.`-prefixed tag from a side branch is passed over
/// rather than installed.
pub async fn fetch_latest_release_tag(
    arch: &str,
    channel: ReleaseChannel,
) -> Result<ReleaseListing, CommandFailure> {
    let client = reqwest::Client::builder()
        .timeout(LISTING_DEADLINE)
        .build()
        .map_err(|error| CommandFailure::generic(error.to_string()))?;
    let response = client
        .get(RELEASE_API_URL)
        .header("accept", "application/vnd.github+json")
        .header("user-agent", GITHUB_USER_AGENT)
        .send()
        .await;
    // Every failure below is still "no release to install", but none is
    // silent: an unreachable or refused listing otherwise reads exactly like
    // an empty one, and a `403` from a missing user agent went unnoticed that
    // way.
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            warn!(%error, "the release listing is unreachable; treating it as empty");
            return Ok(ReleaseListing::none());
        }
    };
    if !response.status().is_success() {
        warn!(
            status = response.status().as_u16(),
            "the release listing was refused; treating it as empty"
        );
        return Ok(ReleaseListing::none());
    }
    let value = match response.json::<serde_json::Value>().await {
        Ok(value) => value,
        Err(error) => {
            warn!(%error, "the release listing is not readable; treating it as empty");
            return Ok(ReleaseListing::none());
        }
    };
    for tag in installable_tags(&value, channel) {
        match tag_is_on_v3(&client, tag).await {
            Some(true) => {
                return Ok(ReleaseListing {
                    tag: tag.to_owned(),
                    arch: arch.to_string(),
                });
            }
            Some(false) => {
                info!(tag, "release is not cut from v3; trying the next candidate");
            }
            None => {
                warn!(
                    tag,
                    "could not ask GitHub whether the release is cut from v3; installing it"
                );
                return Ok(ReleaseListing {
                    tag: tag.to_owned(),
                    arch: arch.to_string(),
                });
            }
        }
    }
    Ok(ReleaseListing::none())
}

/// Whether `tag` was cut from `v3`, asked of GitHub's compare endpoint.
///
/// `None` is "the origin did not answer", and the caller installs the candidate
/// rather than refusing every update on a rate limit.
async fn tag_is_on_v3(client: &reqwest::Client, tag: &str) -> Option<bool> {
    let response = client
        .get(format!("{RELEASE_COMPARE_URL}/v3...{tag}"))
        .header("accept", "application/vnd.github+json")
        .header("user-agent", GITHUB_USER_AGENT)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.json::<serde_json::Value>().await.ok()?;
    compare_status_allows(body.get("status")?.as_str()?)
}
