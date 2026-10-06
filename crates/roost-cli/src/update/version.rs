//! Release versions in semver order, and the channel that decides whether a
//! pre-release is a candidate. Called by `update::mod` (whether to update) and
//! `update::release` (which listed tag to pick); `install.sh` applies the same
//! rules in shell. Depends on `roost-host` for the environment and the dev stamp.

use std::cmp::Ordering;

use roost_host::EnvSource;

use crate::command_error::CommandFailure;

/// The variable that opts a machine into, or out of, pre-releases. Read by
/// `roost update` here and by `install.sh` under the same name.
pub const RELEASE_CHANNEL_ENV: &str = "ROOST_RELEASE_CHANNEL";

/// Which releases are candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseChannel {
    /// Only releases with no pre-release component.
    Stable,
    /// Every release, `-rc.N` included.
    Prerelease,
}

/// One parsed release version. Build metadata (`+sha`) is dropped on parse: a
/// rebuild of one release is the same release, or a binary would update itself
/// forever against a release that has not changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVersion {
    core: [u64; 3],
    pre_release: Vec<PreReleaseIdentifier>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PreReleaseIdentifier {
    Numeric(u64),
    Alphanumeric(String),
}

impl ReleaseVersion {
    /// Parse `v3.1.0`, `3.1.0-rc.2`, or `v3.1.0-rc.2+9f2c1ab`.
    pub fn parse(version: &str) -> Result<Self, CommandFailure> {
        let refuse = || CommandFailure::generic(format!("{version:?} is not a release version"));
        let trimmed = version.trim().trim_start_matches(['v', 'V']);
        let without_build = trimmed.split('+').next().unwrap_or_default();
        let (core, pre_release) = match without_build.split_once('-') {
            Some((core, pre_release)) => (core, Some(pre_release)),
            None => (without_build, None),
        };
        let parts = core
            .split('.')
            .map(|part| {
                (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                    .then(|| part.parse::<u64>().ok())
                    .flatten()
            })
            .collect::<Option<Vec<u64>>>()
            .ok_or_else(refuse)?;
        let core: [u64; 3] = parts.try_into().map_err(|_| refuse())?;
        let pre_release = match pre_release {
            None => Vec::new(),
            Some(text) => text
                .split('.')
                .map(|identifier| {
                    if identifier.is_empty()
                        || !identifier.bytes().all(|byte| byte.is_ascii_alphanumeric())
                    {
                        None
                    } else if identifier.bytes().all(|byte| byte.is_ascii_digit()) {
                        identifier.parse().ok().map(PreReleaseIdentifier::Numeric)
                    } else {
                        Some(PreReleaseIdentifier::Alphanumeric(identifier.to_string()))
                    }
                })
                .collect::<Option<Vec<_>>>()
                .ok_or_else(refuse)?,
        };
        Ok(Self { core, pre_release })
    }

    /// Whether this is a pre-release (`-rc.N` and the like).
    pub fn is_prerelease(&self) -> bool {
        !self.pre_release.is_empty()
    }

    /// Whether a machine on `channel` may install this version.
    pub fn is_on(&self, channel: ReleaseChannel) -> bool {
        channel == ReleaseChannel::Prerelease || !self.is_prerelease()
    }
}

impl std::fmt::Display for ReleaseVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let [major, minor, patch] = self.core;
        write!(formatter, "{major}.{minor}.{patch}")?;
        for (index, identifier) in self.pre_release.iter().enumerate() {
            formatter.write_str(if index == 0 { "-" } else { "." })?;
            match identifier {
                PreReleaseIdentifier::Numeric(number) => write!(formatter, "{number}")?,
                PreReleaseIdentifier::Alphanumeric(text) => formatter.write_str(text)?,
            }
        }
        Ok(())
    }
}

impl Ord for PreReleaseIdentifier {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Numeric(left), Self::Numeric(right)) => left.cmp(right),
            (Self::Numeric(_), Self::Alphanumeric(_)) => Ordering::Less,
            (Self::Alphanumeric(_), Self::Numeric(_)) => Ordering::Greater,
            (Self::Alphanumeric(left), Self::Alphanumeric(right)) => left.cmp(right),
        }
    }
}

impl PartialOrd for PreReleaseIdentifier {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Semver precedence: `3.0.0-rc.9 < 3.0.0-rc.10 < 3.0.0 < 3.0.1-rc.1`.
impl Ord for ReleaseVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            match (self.is_prerelease(), other.is_prerelease()) {
                (false, false) => Ordering::Equal,
                (false, true) => Ordering::Greater,
                (true, false) => Ordering::Less,
                (true, true) => self.pre_release.cmp(&other.pre_release),
            }
        })
    }
}

impl PartialOrd for ReleaseVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The channel this machine updates on.
///
/// `ROOST_RELEASE_CHANNEL=stable|prerelease` decides when set. Unset, a binary
/// that is itself a pre-release stays on pre-releases — it was installed from
/// that track — and every other binary takes stable releases only, so a machine
/// on `3.0.0` is never moved onto `3.0.1-rc.1`. Any other value is refused
/// rather than guessed at.
pub fn release_channel(
    env: &dyn EnvSource,
    current_version: &str,
) -> Result<ReleaseChannel, CommandFailure> {
    match env.get(RELEASE_CHANNEL_ENV).as_deref().map(str::trim) {
        Some("stable") => Ok(ReleaseChannel::Stable),
        Some("prerelease") => Ok(ReleaseChannel::Prerelease),
        None | Some("") => Ok(
            if ReleaseVersion::parse(current_version).is_ok_and(|version| version.is_prerelease()) {
                ReleaseChannel::Prerelease
            } else {
                ReleaseChannel::Stable
            },
        ),
        Some(other) => Err(CommandFailure::generic(format!(
            "{RELEASE_CHANNEL_ENV}={other:?} is not a channel; use `stable` or `prerelease`"
        ))),
    }
}

/// The release version a tag names, with any build metadata removed.
pub fn canonical_release_version(version: &str) -> Result<String, CommandFailure> {
    ReleaseVersion::parse(version).map(|parsed| parsed.to_string())
}

/// Whether the published release is strictly newer than the running binary.
///
/// A source checkout is always behind: it is not a published artifact, and
/// there is nothing to compare it against. An empty tag is never newer, because
/// a listing that named no release is a question this command could not
/// answer. An older or equal release is never an update: a re-cut tag or a
/// late candidate published after a final must not move a machine backwards.
pub fn needs_update(current: &str, latest_tag: &str) -> Result<bool, CommandFailure> {
    if latest_tag.is_empty() {
        return Ok(false);
    }
    if current == roost_host::DEV_BUILD_STAMP {
        return Ok(true);
    }
    Ok(ReleaseVersion::parse(latest_tag)? > ReleaseVersion::parse(current)?)
}
