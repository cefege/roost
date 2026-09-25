//! The log level vocabulary and its ordering. Both are a contract: `roost
//! status` and `roost doctor` parse `level` out of every line, and the
//! minimum-level gate is resolved once at startup. The declaration order IS
//! the wire order (debug=0 … error=3), so it is also the `Ord` order.

use std::fmt;

/// One level of the four-level vocabulary. No `trace`: the wire shape has
/// exactly four values and a fifth would be invisible to every consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    /// Every level, quietest first.
    pub const ALL: [LogLevel; 4] = [Self::Debug, Self::Info, Self::Warn, Self::Error];

    /// The spelling that goes on the wire, lowercase.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    /// Parse a level name. Case-sensitive, like the wire: `DEBUG` is not a
    /// level name here, and neither is `trace`.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "debug" => Some(Self::Debug),
            "info" => Some(Self::Info),
            "warn" => Some(Self::Warn),
            "error" => Some(Self::Error),
            _ => None,
        }
    }

    /// The `ROOST_LOG_LEVEL` resolution: an absent, empty or unrecognized
    /// value falls back to `info` rather than silencing the process.
    pub fn from_env_name(value: Option<&str>) -> Self {
        value.and_then(Self::from_name).unwrap_or(Self::Info)
    }

    /// The `tracing` level this one is gated and named by. `warn` and `error`
    /// are the stderr channel `roost doctor` reads; which stream a level
    /// reaches is [`crate::init::output_stream`]'s single rule.
    pub(crate) fn as_tracing_level(self) -> tracing::Level {
        match self {
            Self::Debug => tracing::Level::DEBUG,
            Self::Info => tracing::Level::INFO,
            Self::Warn => tracing::Level::WARN,
            Self::Error => tracing::Level::ERROR,
        }
    }
}

impl fmt::Display for LogLevel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::LogLevel;

    #[test]
    fn ordering_is_quiet_to_loud() {
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
        assert_eq!(LogLevel::ALL[0], LogLevel::Debug);
        assert_eq!(LogLevel::ALL[3], LogLevel::Error);
    }

    #[test]
    fn names_round_trip_and_render_lowercase() {
        for level in LogLevel::ALL {
            assert_eq!(LogLevel::from_name(level.as_str()), Some(level));
            assert_eq!(level.to_string(), level.as_str());
        }
        assert_eq!(LogLevel::Info.as_str(), "info");
    }

    #[test]
    fn an_unrecognized_level_falls_back_to_info() {
        assert_eq!(LogLevel::from_env_name(None), LogLevel::Info);
        assert_eq!(LogLevel::from_env_name(Some("")), LogLevel::Info);
        assert_eq!(LogLevel::from_env_name(Some("DEBUG")), LogLevel::Info);
        assert_eq!(LogLevel::from_env_name(Some("trace")), LogLevel::Info);
        assert_eq!(LogLevel::from_env_name(Some("verbose")), LogLevel::Info);
        assert_eq!(LogLevel::from_env_name(Some("error")), LogLevel::Error);
    }
}
