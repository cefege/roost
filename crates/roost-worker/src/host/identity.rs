//! The static machine display identity this worker reports: hardware model,
//! chip, distribution. Collected once per process and shared by registration
//! and the heartbeat, so the two cannot describe two different machines.
//! Depends on `roost_protocol::wire` for the record AND its normaliser — and on
//! nothing here.
//!
//! The normaliser is the protocol's, not a second one: a field that passed this
//! crate's rules and failed the coordinator's would be a display value that
//! vanishes at the boundary, and the boundary is the only place it is read.
//!
//! SERIALS NEVER ENTER HERE. A machine's serial number is an inventory
//! identifier, not a display value, and it has no field to travel in.

use std::path::PathBuf;
use std::sync::LazyLock;

use roost_host::HostPlatform;
use roost_protocol::wire::{HostIdentity, normalize_host_identity, normalize_host_identity_text};

/// How much of a source file is read. `os-release` is a few hundred bytes and
/// `sysctl` answers one line; the bound is a ceiling, not a target.
const SOURCE_MAX_BYTES: usize = 8 * 1024;

/// A `sysctl` brand string that is an Apple silicon part. Intel Macs report
/// `Intel(R) Core(TM) i9…` here, which is not a chip name and is dropped.
fn is_apple_chip(value: &str) -> bool {
    let Some(rest) = value.strip_prefix("Apple M") else {
        return false;
    };
    let Some(digits) = rest.chars().next() else {
        return false;
    };
    if !digits.is_ascii_digit() {
        return false;
    }
    rest[1..].is_empty()
        || [" Pro", " Max", " Ultra"]
            .iter()
            .any(|tier| rest.ends_with(tier))
}

/// The value of one `key=value` line in an `os-release` file.
///
/// Quoted values are unescaped for the four escapes the specification gives a
/// `os-release` (`\\`, `\"`, `\$`, `` \` ``); anything else keeps its backslash,
/// because a specification that does not define an escape must not have one
/// silently removed.
#[must_use]
pub fn os_release_value<'a>(source: &'a str, key: &str) -> Option<&'a str> {
    source.lines().find_map(|line| {
        let value = line.strip_prefix(key)?.strip_prefix('=')?;
        if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
            Some(unescape(&value[1..value.len() - 1]))
        } else {
            Some(value.trim())
        }
    })
}

fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut characters = value.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some(escaped @ ('\\' | '"' | '$' | '`')) => out.push(escaped),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// Where each platform's facts come from. Injectable so a collection is
/// testable on a machine that is not the one under test, and so a Windows arm
/// can be a refusal rather than a compile error on POSIX.
pub trait IdentitySources {
    /// `sysctl -n <name>` on macOS, or `None` when it cannot be run.
    fn sysctl(&self, name: &str) -> Option<String>;
    /// The contents of `/etc/os-release` on Linux, or `None`.
    fn os_release(&self) -> Option<String>;
    /// The hardware model. v3 reads no Windows source: a Windows worker does
    /// not exist, and this is the arm that would have read it.
    fn hardware_model(&self) -> Option<String> {
        None
    }
}

/// The sources that read this host.
#[derive(Debug, Default, Clone, Copy)]
pub struct HostIdentitySources;

impl IdentitySources for HostIdentitySources {
    fn sysctl(&self, name: &str) -> Option<String> {
        let out = std::process::Command::new("/usr/sbin/sysctl")
            .arg("-n")
            .arg(name)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8(out.stdout).ok()
    }

    fn os_release(&self) -> Option<String> {
        read_bounded(&PathBuf::from("/etc/os-release"))
    }

    fn hardware_model(&self) -> Option<String> {
        None
    }
}

/// Read at most [`SOURCE_MAX_BYTES`] of a file as UTF-8.
fn read_bounded(path: &std::path::Path) -> Option<String> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buffer = vec![0_u8; SOURCE_MAX_BYTES];
    let read = file.read(&mut buffer).ok()?;
    buffer.truncate(read);
    String::from_utf8(buffer).ok()
}

/// The identity of the machine under `platform`, or `None` when nothing about
/// it survived normalisation.
pub fn collect_host_identity(
    platform: HostPlatform,
    sources: &dyn IdentitySources,
) -> Option<HostIdentity> {
    if platform == HostPlatform::Windows {
        // A refusal, not a default: v3 has no Windows sampler, so there is no
        // honest answer to give for a host v3 does not run on.
        return None;
    }
    let chip = sources
        .sysctl("machdep.cpu.brand_string")
        .and_then(|value| normalize_host_identity_text(&serde_json::Value::String(value)))
        .filter(|chip| is_apple_chip(chip));
    let identity = match platform {
        HostPlatform::MacOs => HostIdentity {
            hardware_model: sources.sysctl("hw.model"),
            chip,
            linux_distribution: None,
        },
        _ => {
            let distribution = sources.os_release().and_then(|source| {
                os_release_value(&source, "PRETTY_NAME")
                    .or_else(|| os_release_value(&source, "NAME"))
                    .and_then(|value| {
                        normalize_host_identity_text(&serde_json::Value::String(value.to_string()))
                    })
            });
            HostIdentity {
                hardware_model: None,
                chip: None,
                linux_distribution: distribution,
            }
        }
        HostPlatform::Windows => return None,
    };
    normalize_host_identity(&serde_json::to_value(identity).ok()?)
}

/// The identity of this machine, collected once.
///
/// A `LazyLock` rather than a mutable global: the collection shells out to
/// `sysctl` and reads a file, and a worker that paid for it per heartbeat would
/// spawn a process every thirty seconds to learn a value that cannot change
/// while the process runs.
pub fn static_host_identity() -> Option<HostIdentity> {
    static CACHED: LazyLock<Option<HostIdentity>> = LazyLock::new(|| {
        let platform = roost_host::supported_host_platform().ok()?;
        collect_host_identity(platform, &HostIdentitySources)
    });
    CACHED.clone()
}
