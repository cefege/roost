//! The three host platforms a worker can run on, and the one exhaustive
//! match every platform decision goes through. There is no default argument
//! and no ambient platform: the platform is a parameter, so a coordinator on
//! macOS can reason about a worker's Windows path instead of reading its own.

use std::fmt;

use thiserror::Error;

/// A host this product supports. A machine on anything else is refused at the
/// edge rather than mis-served by a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HostPlatform {
    MacOs,
    Linux,
    Windows,
}

/// A platform name this product does not support.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unsupported host platform: {0}")]
pub struct PlatformError(pub String);

impl HostPlatform {
    /// Every supported platform.
    pub const ALL: [HostPlatform; 3] = [Self::MacOs, Self::Linux, Self::Windows];

    /// The wire name, as `process.platform` spelled it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MacOs => "darwin",
            Self::Linux => "linux",
            Self::Windows => "win32",
        }
    }

    /// Whether a string names a platform this product supports. The test a
    /// caller holding a raw `os` column needs before it may fold anything.
    pub fn is_supported(value: &str) -> bool {
        Self::parse(value).is_ok()
    }

    /// Resolve a platform name without letting an unknown host fall through.
    pub fn parse(value: &str) -> Result<Self, PlatformError> {
        match value {
            "darwin" => Ok(Self::MacOs),
            "linux" => Ok(Self::Linux),
            "win32" => Ok(Self::Windows),
            other => Err(PlatformError(if other.is_empty() {
                "unknown".to_owned()
            } else {
                other.to_owned()
            })),
        }
    }

    /// The name an operator reads.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::MacOs => "macOS",
            Self::Linux => "Linux",
            Self::Windows => "Windows",
        }
    }

    /// The platform this binary was built for, decided at compile time from
    /// the target. `None` on a target this product does not support.
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::MacOs)
        } else if cfg!(target_os = "linux") {
            Some(Self::Linux)
        } else if cfg!(target_os = "windows") {
            Some(Self::Windows)
        } else {
            None
        }
    }

    /// The named failure of an arm a match was meant to be exhaustive. An
    /// exhaustive `match` on this enum has no other arm, so this is where a
    /// future variant is refused instead of silently mapped.
    pub fn assert_never(self) -> ! {
        panic!("unhandled host platform: {}", self.as_str())
    }
}

impl fmt::Display for HostPlatform {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::HostPlatform;

    #[test]
    fn the_three_wire_names_resolve_and_nothing_else_does() {
        for (name, platform) in [
            ("darwin", HostPlatform::MacOs),
            ("linux", HostPlatform::Linux),
            ("win32", HostPlatform::Windows),
        ] {
            assert_eq!(HostPlatform::parse(name), Ok(platform));
            assert_eq!(platform.as_str(), name);
            assert!(HostPlatform::is_supported(name));
        }
        assert_eq!(HostPlatform::ALL.len(), 3);
    }

    #[test]
    fn an_unknown_host_is_named_rather_than_defaulted() {
        for unknown in ["", "plan9", "freebsd", "Darwin", "linux2"] {
            let error = HostPlatform::parse(unknown).expect_err("unsupported");
            let expected = if unknown.is_empty() {
                "unknown"
            } else {
                unknown
            };
            assert_eq!(
                error.to_string(),
                format!("unsupported host platform: {expected}")
            );
            assert!(!HostPlatform::is_supported(unknown));
        }
    }

    #[test]
    fn the_display_names_are_what_an_operator_reads() {
        assert_eq!(HostPlatform::MacOs.display_name(), "macOS");
        assert_eq!(HostPlatform::Linux.display_name(), "Linux");
        assert_eq!(HostPlatform::Windows.display_name(), "Windows");
    }

    #[test]
    fn the_build_target_is_a_supported_platform() {
        let current = HostPlatform::current().expect("this product builds for a supported target");
        assert!(HostPlatform::ALL.contains(&current));
    }

    #[test]
    #[should_panic(expected = "unhandled host platform")]
    fn the_exhaustive_arm_names_the_platform_it_refuses() {
        HostPlatform::Linux.assert_never();
    }
}
