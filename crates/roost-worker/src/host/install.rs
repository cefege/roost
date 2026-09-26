//! Erasure of a one-shot environment entry from this worker's installed
//! service definition: the LaunchAgent plist on macOS, the systemd --user unit
//! on Linux. Called by the activation that SPENDS a one-shot authorization —
//! the redeemed bootstrap token, the keeper force-live retire — and by nothing
//! else. Depends on `roost_host::worker_service_path` and nothing that boots,
//! so erasing a key never drags the service up with it.
//!
//! WINDOWS IS NOT PORTED. v2 returned early on `win32` and did the equivalent
//! through a service DACL instead (`apps/worker/src/host/service-definition-env.ts:26`),
//! so there is nothing here to stub: a Windows worker does not exist in v3, and
//! a platform with no definition to edit is a refusal.
//!
//! The definition itself is written by `roost-cli`'s service installer, the one
//! owner of what a definition contains. This erases one entry from it.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use roost_host::{EnvSource, HostPlatform, supported_host_platform, worker_service_path};
use roost_platform::KEEPER_FORCE_LIVE_RETIRE_ENV;

/// The one-shot token an install redeems. Named here so the value that is
/// erased and the value that was redeemed cannot drift apart in two modules.
pub const BOOTSTRAP_TOKEN_ENV: &str = "ROOST_BOOTSTRAP_TOKEN";

/// The mode an edited definition is left at: it is a file an operator may have
/// to read during an incident, and it is also a file a secret was just removed
/// from.
const DEFINITION_MODE: u32 = 0o600;

/// Which kind of service definition an erasure is against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceDefinition {
    /// A macOS LaunchAgent property list.
    LaunchAgent,
    /// A Linux systemd --user unit.
    SystemdUserUnit,
}

impl ServiceDefinition {
    /// The definition kind a platform's worker runs under.
    pub fn for_platform(platform: HostPlatform) -> Result<Self, InstallError> {
        match platform {
            HostPlatform::MacOs => Ok(Self::LaunchAgent),
            HostPlatform::Linux => Ok(Self::SystemdUserUnit),
            HostPlatform::Windows => Err(InstallError::UnsupportedPlatform {
                platform: platform.as_str(),
            }),
        }
    }

    /// Whether an edit has to be handed to the service manager before it takes
    /// effect. systemd serves a unit's cached contents until it is reloaded;
    /// launchd re-reads the plist on the next launch, so telling launchd would
    /// be a second process spawned to be ignored.
    #[must_use]
    pub const fn needs_reload(self) -> bool {
        matches!(self, Self::SystemdUserUnit)
    }
}

/// Why a one-shot entry could not be erased.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error("the worker's service definition path could not be resolved: {reason}")]
    Unresolved { reason: String },
    #[error("the worker service definition at {path} could not be read: {reason}")]
    Unreadable { path: PathBuf, reason: String },
    #[error("the worker service definition at {path} could not be rewritten: {reason}")]
    Unwritable { path: PathBuf, reason: String },
}

/// Remove the entry for `key` from the worker's installed service definition.
/// `true` when the definition carried it.
///
/// `false` is a real answer, not a failure: a value supplied by the ambient
/// shell of whoever started the service by hand was never in the file, and
/// erasing nothing is what a correct erasure looks like from out here.
pub async fn scrub_service_definition_env(
    env: &dyn EnvSource,
    platform: HostPlatform,
    key: &str,
) -> Result<bool, InstallError> {
    if !is_env_name(key) {
        return Err(InstallError::NotAnEnvName {
            key: key.to_string(),
        });
    }
    let definition = ServiceDefinition::for_platform(platform)?;
    let path = worker_service_path(env, platform).map_err(|error| InstallError::Unresolved {
        reason: error.to_string(),
    })?;
    let raw = std::fs::read_to_string(&path).map_err(|error| InstallError::Unreadable {
        path: path.clone(),
        reason: error.to_string(),
    })?;
    let next = match definition {
        ServiceDefinition::LaunchAgent => erase_plist_entry(&raw, key),
        ServiceDefinition::SystemdUserUnit => erase_unit_entry(&raw, key),
    };
    if next == raw {
        return Ok(false);
    }
    replace_definition(&path, &next)?;
    if definition.needs_reload() {
        reload_unit_manager(&path).await;
    }
    Ok(true)
}

/// Spend the keeper force-live retire authorization: erase it from the
/// definition, and record whether it was there.
///
/// The authorization ends every PTY a keeper holds, so it is valid for exactly
/// the activation that received it. A value left in the definition would
/// re-authorize that destruction on every later restart, long after the
/// operator who typed the flag stopped watching. The failure to erase is
/// logged rather than returned: the PTYs this activation did not retire are
/// still running, and refusing to boot is a larger incident than the one being
/// prevented.
///
/// The platform is resolved here rather than taken: `serve_until` has none of
/// its own to hand in — boot resolved every path through one and did not keep
/// it — and widening `WorkerBoot` with a field two callers would then have to
/// agree about is a worse answer than reading the host's own answer.
pub async fn spend_keeper_force_live_retire_authorization(env: &dyn EnvSource) {
    let platform = match supported_host_platform() {
        Ok(platform) => platform,
        Err(error) => {
            tracing::error!(
                %error,
                "the keeper force-live retire authorization could NOT be spent: the host platform \
                 this binary runs on is not one v3 supports"
            );
            return;
        }
    };
    match scrub_service_definition_env(env, platform, KEEPER_FORCE_LIVE_RETIRE_ENV).await {
        Ok(removed) => tracing::warn!(
            key = KEEPER_FORCE_LIVE_RETIRE_ENV,
            removed_from_service_definition = removed,
            "the keeper force-live retire authorization is spent and will not survive a restart"
        ),
        Err(error) => tracing::error!(
            key = KEEPER_FORCE_LIVE_RETIRE_ENV,
            %error,
            "the keeper force-live retire authorization could NOT be spent; it will authorize \
             the same retirement again on the next start"
        ),
    }
}

/// The `<key>K</key><string>V</string>` pair a plist carries, on one line or on
/// two, with the whitespace in front of it. The definition writer puts each tag
/// on its own line and a hand-edited plist puts both on one, so both are read.
///
/// A `<key>` with no `<string>` after it is a plist entry of another type and
/// is left alone: a boolean next to the environment dictionary has nothing to do
/// with this key and deleting it would corrupt a definition nobody asked about.
fn erase_plist_entry(raw: &str, key: &str) -> String {
    const STRING_OPEN: &str = "<string>";
    const STRING_CLOSE: &str = "</string>";
    let opening = format!("<key>{key}</key>");
    let mut erased = String::with_capacity(raw.len());
    let mut cursor = 0;
    while let Some(found) = raw[cursor..].find(&opening) {
        let open_at = cursor + found;
        let after_open = open_at + opening.len();
        let body = raw[after_open..].trim_start();
        let value_at = after_open + raw[after_open..].len() - body.len();
        let Some(value) = body.strip_prefix(STRING_OPEN) else {
            erased.push_str(&raw[cursor..after_open]);
            cursor = after_open;
            continue;
        };
        let Some(end) = value.find(STRING_CLOSE) else {
            erased.push_str(&raw[cursor..after_open]);
            cursor = after_open;
            continue;
        };
        let value_end = value_at + STRING_OPEN.len() + end + STRING_CLOSE.len();
        erased.push_str(&raw[cursor..raw[..open_at].trim_end().len()]);
        cursor = value_end;
    }
    erased.push_str(&raw[cursor..]);
    erased
}

/// The `Environment=` entries of a systemd unit, with `key` removed and every
/// other entry kept.
///
/// A systemd line holds a space-separated list of quoted pairs, so dropping the
/// LINE the way a line filter would take unrelated variables with it: an erased
/// `ROOST_KEEPER_FORCE_LIVE_RETIRE` would take `ROOST_COORDINATOR_URL` with it
/// on any definition that put two pairs on one line.
fn erase_unit_entry(raw: &str, key: &str) -> String {
    let mut erased = String::with_capacity(raw.len());
    for line in raw.split_inclusive('\n') {
        let Some(pairs) = environment_pairs(line) else {
            erased.push_str(line);
            continue;
        };
        let kept: Vec<&EnvironmentPair<'_>> =
            pairs.iter().filter(|pair| pair.name != key).collect();
        if kept.is_empty() {
            continue;
        }
        if kept.len() == pairs.len() {
            // Nothing on this line is the key, so the line is not rewritten.
            // Re-emitting a pair means re-escaping its value, and a value the
            // installer wrote as `\t` or `%%` comes back as a `t` or a `%` the
            // second time it is decoded. The survivors keep their own bytes.
            erased.push_str(line);
            continue;
        }
        let mut rebuilt = String::from(&line[..pairs[0].start]);
        for pair in kept {
            rebuilt.push(' ');
            rebuilt.push_str(&line[pair.start..pair.end]);
        }
        if line.ends_with('\n') {
            rebuilt.push('\n');
        }
        erased.push_str(&rebuilt);
    }
    erased
}

/// One `NAME=VALUE` pair on an `Environment=` line, and where it sits in the
/// line's own bytes. The span is what makes the erase surgical: the survivors
/// are copied across untouched rather than decoded and written back.
struct EnvironmentPair<'a> {
    name: &'a str,
    start: usize,
    end: usize,
}

/// The pairs of an `Environment=` line, or `None` when the line is anything
/// else. Both spellings are on disk: the installer writes `Environment="A=1"`
/// and a hand-edited unit says `Environment=A=1`.
fn environment_pairs(line: &str) -> Option<Vec<EnvironmentPair<'_>>> {
    let indent = line.len() - line.trim_start().len();
    let list = line[indent..].strip_prefix("Environment=")?;
    let list_start = indent + "Environment=".len();
    let mut pairs = Vec::new();
    let mut cursor = 0;
    while cursor < list.len() {
        if list.as_bytes()[cursor] == b' ' {
            cursor += 1;
            continue;
        }
        let (end, next) = match list[cursor..].strip_prefix('"') {
            Some(quoted) => closing_quote(&list[cursor + 1..], cursor + 1)?,
            None => {
                let offset = list[cursor..]
                    .find(' ')
                    .unwrap_or(list.len() - cursor);
                (cursor + offset, cursor + offset)
            }
        };
        if let Some((name, _)) = list[cursor..end].split_once('=') {
            pairs.push(EnvironmentPair {
                name,
                start: list_start + cursor,
                end: list_start + end,
            });
        }
        cursor = next;
    }
    (!pairs.is_empty()).then_some(pairs)
}

/// The byte just past a quoted value's closing quote, given where the opening
/// quote sat. A `\"` inside the value is part of the value, not the end of it.
fn closing_quote(value: &str, opened_at: usize) -> Option<(usize, usize)> {
    let mut characters = value.char_indices();
    while let Some((index, character)) = characters.next() {
        match character {
            '\\' => {
                characters.next()?;
            }
            '"' => return Some((opened_at + index + 1, opened_at + index + 1)),
            _ => {}
        }
    }
    None
}

/// An environment name as a service definition spells one. The name is
/// interpolated into the text both erasers match against, so a value that is
/// not a plain name is refused rather than edited by a rule nobody wrote.
fn is_env_name(key: &str) -> bool {
    let mut characters = key.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_uppercase())
        && characters.all(|character| {
            character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
        })
}

/// Write the edited definition beside its own name and move it into place, so a
/// reader never sees half a definition and a failed write leaves the original.
fn replace_definition(path: &Path, next: &str) -> Result<(), InstallError> {
    let unwritable = |reason: String| InstallError::Unwritable {
        path: path.to_path_buf(),
        reason,
    };
    let mut staged = path.as_os_str().to_os_string();
    staged.push(".env-scrub");
    let staged = PathBuf::from(staged);
    std::fs::write(&staged, next).map_err(|error| unwritable(error.to_string()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(DEFINITION_MODE))
        .map_err(|error| unwritable(error.to_string()))?;
    std::fs::rename(&staged, path).map_err(|error| unwritable(error.to_string()))
}

/// Hand systemd the edited unit. A failure is logged and not returned: the entry
/// is gone from the file either way, and the next start reads the file.
async fn reload_unit_manager(path: &Path) {
    match tokio::process::Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            tracing::info!(path = %path.display(), "the edited unit was handed to systemd");
        }
        Ok(output) => tracing::warn!(
            path = %path.display(),
            status = %output.status,
            "systemd did not accept the edited unit; the entry is still gone from the file"
        ),
        Err(error) => tracing::warn!(
            path = %path.display(),
            %error,
            "systemd could not be reached to reload the edited unit; the entry is still gone \
             from the file"
        ),
    }
}
