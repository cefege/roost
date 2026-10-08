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

/// The Windows profile directory, the home fallback when `HOME` is unset.
pub const USERPROFILE_ENV: &str = "USERPROFILE";

/// The Windows per-user, per-machine data root.
pub const LOCAL_APP_DATA_ENV: &str = "LOCALAPPDATA";

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
pub trait EnvSource: Sync {
    /// The value of `key`, or `None` when it is not set at all.
    fn get(&self, key: &str) -> Option<String>;

    /// The user's home directory (`HOME`, else `USERPROFILE`), or `None` when
    /// neither is set.
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
        self.get(HOME_ENV)
            .or_else(|| self.get(USERPROFILE_ENV))
            .map(PathBuf::from)
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
        self.get(HOME_ENV)
            .or_else(|| self.get(USERPROFILE_ENV))
            .map(PathBuf::from)
    }
}

/// The host this process is running on, or a refusal with a reason.
///
/// The platform is the build target's ([`HostPlatform::current`]), never a parse
/// of `std::env::consts::OS`: Rust names macOS `macos` where the wire name
/// [`HostPlatform::parse`] reads is `darwin`, and that parse refused every Mac
/// at boot. A target the product does not name is refused.
pub fn supported_host_platform() -> ProtocolResult<HostPlatform> {
    HostPlatform::current().ok_or_else(|| {
        ProtocolError::new(
            "host.platform",
            format!("unsupported host platform: {}", std::env::consts::OS),
        )
    })
}

/// The same resolution from a name a caller already holds — a test, a deploy
/// manifest, or a column in the worker registry.
pub fn host_platform_from_os(os: &str) -> ProtocolResult<HostPlatform> {
    HostPlatform::parse(os).map_err(|error| {
        ProtocolError::new(
            "host.platform",
            format!("unsupported host platform: {error}"),
        )
    })
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
        let windows = MapEnv::new().with("USERPROFILE", r"C:\Users\op");
        assert_eq!(windows.home_dir(), Some(r"C:\Users\op".into()));
    }

    #[test]
    fn every_named_host_resolves_and_an_unknown_one_is_refused() {
        assert_eq!(
            host_platform_from_os("darwin").map(|platform| platform.display_name().to_string()),
            Ok("macOS".to_string())
        );
        assert_eq!(
            host_platform_from_os("linux").map(|platform| platform.display_name().to_string()),
            Ok("Linux".to_string())
        );
        assert_eq!(
            host_platform_from_os("win32").map(|platform| platform.display_name().to_string()),
            Ok("Windows".to_string())
        );
        assert!(host_platform_from_os("").is_err());
    }
}
