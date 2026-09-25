//! The environment and the platform a path or a boot config resolves against.
//!
//! Both are injected, never read from the process inside a path function. The
//! home directory in particular is the one value a test most needs to fake, and
//! a path function that called `std::env::var` itself could only ever be
//! exercised on the machine that ran the test.

use std::collections::BTreeMap;
use std::path::PathBuf;

use roost_platform::HostPlatform;
use roost_protocol::{ProtocolError, ProtocolResult};

/// The home-directory variable every default path hangs off.
pub const HOME_ENV: &str = "HOME";

/// The Linux data root, per the XDG base-directory specification.
pub const XDG_DATA_HOME_ENV: &str = "XDG_DATA_HOME";

/// The Linux state root; logs live here because they are mutable state.
pub const XDG_STATE_HOME_ENV: &str = "XDG_STATE_HOME";

/// The variables a path or config function reads, injectable as one pair.
///
/// `get` separates "set to an empty string" from "absent" because the
/// TypeScript originals used both `??` and a truthiness test on the same
/// variables, and they disagreed: an empty `ROOST_COORD_PLIST` names a real
/// file, while an empty `ROOST_WORKER_DATA_DIR` falls back to the default.
pub trait EnvSource {
    /// The value of `key`, or `None` when it is not set at all.
    fn get(&self, key: &str) -> Option<String>;

    /// The user's home directory, or `None` when it cannot be resolved.
    fn home_dir(&self) -> Option<PathBuf>;
}

/// The real process environment. The only implementation a binary uses.
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnv;

impl ProcessEnv {
    pub const fn new() -> Self {
        Self
    }
}

impl EnvSource for ProcessEnv {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.get(HOME_ENV).map(PathBuf::from)
    }
}

/// A fixed environment for a test, backed by a sorted map so a failure
/// reports the same iteration order on every run.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct MapEnv {
    entries: BTreeMap<String, String>,
}

impl MapEnv {
    pub fn new() -> Self {
        Self::default()
    }

    /// An environment with one more variable set.
    pub fn with(mut self, key: &str, value: &str) -> Self {
        self.set(key, value);
        self
    }

    pub fn set(&mut self, key: &str, value: &str) {
        self.entries.insert(key.to_string(), value.to_string());
    }
}

impl EnvSource for MapEnv {
    fn get(&self, key: &str) -> Option<String> {
        self.entries.get(key).cloned()
    }

    fn home_dir(&self) -> Option<PathBuf> {
        self.get(HOME_ENV).map(PathBuf::from)
    }
}

/// The host this process is running on, or a refusal with a reason.
///
/// This is the one entry point from a raw `os` string to a [`HostPlatform`],
/// and it composes two questions the product answers separately:
/// `HostPlatform::parse` owns "is this a platform we have a name for", and
/// this adds "is it a platform v3 runs on". Windows has a name and is still
/// refused: v3 ships Linux and macOS only, so a Windows host must fail at boot
/// with a clear reason instead of writing into a layout no v3 release installs.
pub fn supported_host_platform() -> ProtocolResult<HostPlatform> {
    let os = std::env::consts::OS;
    let platform = HostPlatform::parse(os).map_err(|error| {
        ProtocolError::new(
            "host.platform",
            format!("unsupported host platform: {error}"),
        )
    })?;
    if platform == HostPlatform::Windows {
        return Err(ProtocolError::new(
            "host.platform",
            "Roost v3 does not support Windows; v3 installs on macOS and Linux only",
        ));
    }
    Ok(platform)
}

/// The same refusal, resolved from a name a caller already holds — a test, a
/// deploy manifest, or a column in the worker registry.
pub fn host_platform_from_os(os: &str) -> ProtocolResult<HostPlatform> {
    let platform = HostPlatform::parse(os).map_err(|error| {
        ProtocolError::new(
            "host.platform",
            format!("unsupported host platform: {error}"),
        )
    })?;
    if platform == HostPlatform::Windows {
        return Err(ProtocolError::new(
            "host.platform",
            "Roost v3 does not support Windows; v3 installs on macOS and Linux only",
        ));
    }
    Ok(platform)
}

#[cfg(test)]
mod tests {
    use super::{EnvSource, MapEnv, host_platform_from_os};

    #[test]
    fn a_fake_environment_answers_absent_and_empty_differently() {
        let env = MapEnv::new().with("ROOST_COORD_LABEL", "");
        assert_eq!(env.get("ROOST_COORD_LABEL").as_deref(), Some(""));
        assert_eq!(env.get("ROOST_WORKER_LABEL"), None);
    }

    #[test]
    fn the_home_directory_comes_from_the_injected_source() {
        let env = MapEnv::new().with("HOME", "/home/operator");
        assert_eq!(env.home_dir(), Some("/home/operator".into()));
        assert_eq!(MapEnv::new().home_dir(), None);
    }

    #[test]
    fn an_unknown_or_unsupported_host_is_refused() {
        assert_eq!(
            host_platform_from_os("darwin").map(|platform| platform.display_name().to_string()),
            Ok("macOS".to_string())
        );
        assert_eq!(
            host_platform_from_os("linux").map(|platform| platform.display_name().to_string()),
            Ok("Linux".to_string())
        );
        assert!(host_platform_from_os("win32").is_err());
        assert!(host_platform_from_os("").is_err());
    }
}
