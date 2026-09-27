//! The two documents that cross the ssh boundary of a deploy, and the only two
//! shapes the deploying side and the target-side driver agree on.
//!
//! [`ApplyManifest`] travels inward: the deploying box states what release it
//! staged and what the installed definition must say, and the target applies it.
//! [`ApplyReport`] travels outward: the target states what it actually did, in
//! terms the deploying box can turn into an exit code without guessing.
//!
//! Note what is NOT in the manifest: the journaled keeper update. In this
//! architecture the keeper action is a conversation between the coordinator and
//! the worker over their own link, fenced by the coordinator's drain, and it
//! happens entirely before and after this manifest is applied. A manifest field
//! for it would be a second delivery mechanism for a value the worker never
//! reads from a file.
//!
//! Both documents are versioned. A target running an older release must refuse a
//! manifest it would silently misread, and a deploying box must refuse a report
//! from a target whose vocabulary it does not have — a report that parsed into a
//! "settled" because the field was unknown is the one failure this pair of
//! documents exists to prevent.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The schema both documents carry.
pub const APPLY_SCHEMA: u32 = 1;

/// What the deploying box hands the target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyManifest {
    /// Always [`APPLY_SCHEMA`].
    pub schema: u32,
    /// The commit this release was built from. The target names the installed
    /// release directory after it, so it has to be a value that can be a
    /// directory name.
    pub git_sha: String,
    /// The absolute path the release was staged at, which the target moves into
    /// its own release directory rather than being told where that is. The
    /// target's release directory comes from its installed service definition,
    /// which is the only place the install's own path policy survives.
    pub staged_dir: String,
    /// Lowercase hex SHA-256 over the staged release tree, which the target
    /// re-reads before it installs anything. A release that changed between
    /// staging and activation is a release nobody proved.
    pub release_digest: String,
    /// The worker definition's environment, already resolved: identity keys from
    /// the invocation or the target's own installed definition, fleet keys from
    /// the invocation, the operator's ambient environment, or the target's own
    /// choice — each decided in identity_env.rs and nowhere else.
    pub environment: BTreeMap<String, String>,
}

impl ApplyManifest {
    /// A manifest for `git_sha`'s release, as the deploying side builds it.
    pub fn new(
        git_sha: &str,
        staged_dir: &str,
        release_digest: &str,
        environment: BTreeMap<String, String>,
    ) -> Self {
        Self {
            schema: APPLY_SCHEMA,
            git_sha: git_sha.to_string(),
            staged_dir: staged_dir.to_string(),
            release_digest: release_digest.to_string(),
            environment,
        }
    }

    /// Encode for the wire. One line, because it is piped: a pretty-printed
    /// manifest would arrive as several lines and the target would read the
    /// first.
    pub fn encode(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|error| error.to_string())
    }

    /// Decode what the deploying side sent, refusing a shape this build does
    /// not have rather than reading the fields it recognises and ignoring the
    /// rest.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let manifest: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        if manifest.schema != APPLY_SCHEMA {
            return Err(format!(
                "apply manifest schema {} is not {APPLY_SCHEMA}; the target's roost is not the \
                 release this deploy was prepared for",
                manifest.schema
            ));
        }
        manifest.validate()?;
        Ok(manifest)
    }

    /// The rules a manifest has to satisfy before anything on the target is
    /// touched. Each one is a value that would otherwise become a directory name
    /// or a unit-file line.
    pub fn validate(&self) -> Result<(), String> {
        if self.git_sha.is_empty()
            || !self
                .git_sha
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '-')
        {
            return Err(format!(
                "apply manifest names a release directory {:?}, which is not a build identity",
                self.git_sha
            ));
        }
        if !self.staged_dir.starts_with('/') {
            return Err(format!(
                "apply manifest staged directory is not absolute: {}",
                self.staged_dir
            ));
        }
        if self.release_digest.len() != 64
            || !self
                .release_digest
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        {
            return Err(format!(
                "apply manifest release digest is not a 64-hex SHA-256: {}",
                self.release_digest
            ));
        }
        Ok(())
    }
}

/// How the target's apply ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyOutcome {
    /// The new definition is installed, the service is running on it, and the
    /// journal is gone. Nothing is left to do.
    Settled,
    /// The new definition did not come up and the previous one is back and
    /// running. The machine is where it started.
    RolledBack,
    /// An earlier deploy had been left in flight and this run put it back; the
    /// release this manifest names was not installed.
    Recovered,
    /// The definition was replaced and the deploy could not be settled after
    /// that. The journal is on disk and the next deploy resolves it. This is the
    /// outcome that is exit 8, because the machine is past the point where
    /// pretending otherwise is honest.
    Unsettled,
    /// Nothing on the target was changed.
    Refused,
}

/// What the target states it did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplyReport {
    /// Always [`APPLY_SCHEMA`].
    pub schema: u32,
    /// How the apply ended.
    pub outcome: ApplyOutcome,
    /// The message an operator reads. Never empty, so a report with no
    /// explanation is a bug the deploying side can name.
    pub detail: String,
    /// Whether this deploy's definition bytes differ from what was installed.
    pub definition_changed: bool,
    /// The absolute path of the installed definition, for the summary.
    pub definition_path: String,
    /// The release directory this deploy installed, once installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_dir: Option<String>,
    /// The release directory the definition pointed at before this deploy, when
    /// there was one. The deploying side retires it only after a settled report
    /// — never before, because a report of failure that still deleted the prior
    /// release would leave a machine with no release at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_release_dir: Option<String>,
}

impl ApplyReport {
    /// A report for `outcome`, with a message.
    pub fn new(outcome: ApplyOutcome, detail: impl Into<String>) -> Self {
        Self {
            schema: APPLY_SCHEMA,
            outcome,
            detail: detail.into(),
            definition_changed: false,
            definition_path: String::new(),
            release_dir: None,
            prior_release_dir: None,
        }
    }

    /// Encode for the wire: one line, no trailing newline.
    pub fn encode(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    /// Read the report out of a remote command's stdout.
    ///
    /// The line is prefixed rather than parsed positionally, because a target
    /// that prints a warning before its report is a target whose report is still
    /// the truth. A missing report is an error rather than an empty one: a deploy
    /// that treated silence as success is the defect this whole path exists to
    /// prevent.
    pub fn decode(output: &str) -> Result<Self, String> {
        let line = output
            .lines()
            .map(str::trim)
            .find(|line| line.starts_with(REPORT_PREFIX))
            .ok_or_else(|| {
                format!(
                    "the target did not report an apply result; its last output was: {}",
                    if output.trim().is_empty() {
                        "(nothing)"
                    } else {
                        output.trim()
                    }
                )
            })?;
        let report: Self = serde_json::from_str(&line[REPORT_PREFIX.len()..])
            .map_err(|error| format!("the target's apply report is unreadable: {error}"))?;
        if report.schema != APPLY_SCHEMA {
            return Err(format!(
                "the target's apply report is schema {} and this is {APPLY_SCHEMA}; its roost is \
                 not the release this deploy was prepared for",
                report.schema
            ));
        }
        if report.detail.trim().is_empty() {
            return Err("the target's apply report carries no explanation".to_string());
        }
        Ok(report)
    }
}

/// The marker the target prefixes its report line with. Named here because the
/// deploying side looks for it by string, which is the same arrangement
/// `roost __keeper-contract` makes.
pub const REPORT_PREFIX: &str = "RoostApplyReport=";
