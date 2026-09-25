//! Process startup for the logging facade: the environment is read here and
//! nowhere else, the level gate lives in the subscriber's filter rather than
//! in a static, and the clock the lines are stamped from is the one the host
//! passed in. Two layers, because the streams are part of the contract:
//! `warn`/`error` to stderr, `debug`/`info` to stdout.

use std::sync::Arc;

use tracing_subscriber::Layer;
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::{SubscriberInitExt, TryInitError};

use crate::clock::{EventClock, SystemClock};
use crate::diag;
use crate::level::LogLevel;
use crate::line::JsonLineFormat;
use crate::runtime::{self, Process};

/// The environment variable that raises or lowers the minimum level. Read
/// once, at startup; an unrecognized value is `info`.
pub const LOG_LEVEL_ENV: &str = "ROOST_LOG_LEVEL";

/// The minimum level used when [`LOG_LEVEL_ENV`] is absent or unrecognized.
pub const DEFAULT_LOG_LEVEL: LogLevel = LogLevel::Info;

/// Which stream one level goes to. `warn` and `error` are the anomaly channel
/// `roost doctor` reads; `debug` and `info` are the operational log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputStream {
    Stdout,
    Stderr,
}

/// Everything `init` needs, so a host — or a test — decides each piece instead
/// of having the environment decide behind it.
#[derive(Debug, Clone)]
pub struct InitOptions {
    min_level: LogLevel,
    diag_enabled: bool,
    clock: Arc<dyn EventClock>,
}

impl InitOptions {
    /// The quiet default: `info`, firehose off, the process clock.
    pub fn new(clock: Arc<dyn EventClock>) -> Self {
        Self {
            min_level: DEFAULT_LOG_LEVEL,
            diag_enabled: false,
            clock,
        }
    }

    pub fn with_min_level(mut self, min_level: LogLevel) -> Self {
        self.min_level = min_level;
        self
    }

    /// The one door for the firehose gate. A parameter and not a setter,
    /// because the decision is read once at startup and must not be
    /// runtime-togglable; the browser front end reads its own storage and
    /// hands the answer here.
    pub fn with_diag_enabled(mut self, diag_enabled: bool) -> Self {
        self.diag_enabled = diag_enabled;
        self
    }
}

/// Read the environment and install the subscriber. The only two environment
/// reads in the crate. A second call fails the way a second global subscriber
/// always does, and leaves the first one in place.
pub fn init() -> Result<(), TryInitError> {
    let min_level = LogLevel::from_env_name(std::env::var(LOG_LEVEL_ENV).ok().as_deref());
    let diag_enabled =
        diag::enabled_from_env_value(std::env::var(diag::DIAG_ENABLED_ENV).ok().as_deref());
    init_with(
        InitOptions::new(Arc::new(SystemClock))
            .with_min_level(min_level)
            .with_diag_enabled(diag_enabled),
    )
}

/// Install the subscriber and publish the process with already-resolved
/// options. Nothing here reads the environment or a clock of its own.
pub fn init_with(options: InitOptions) -> Result<(), TryInitError> {
    runtime::install_process(Process::new(options.diag_enabled, options.clock.clone()));
    subscriber(&options).try_init()
}

/// The two-layer subscriber: one formatter, two writers, two filters. Written
/// as a function rather than inline so a test can install it for one thread
/// and assert on real formatted lines.
pub(crate) fn subscriber(
    options: &InitOptions,
) -> impl tracing::subscriber::Subscriber + Send + Sync + 'static {
    let stdout_min_level = options.min_level;
    let stderr_min_level = options.min_level;
    let stdout_clock = options.clock.clone();
    let stderr_clock = options.clock.clone();
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(JsonLineFormat::new(stdout_clock))
                .with_writer(std::io::stdout)
                .with_filter(filter_fn(move |metadata| {
                    admits(metadata.level(), stdout_min_level)
                        && output_stream(*metadata.level()) == OutputStream::Stdout
                })),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .event_format(JsonLineFormat::new(stderr_clock))
                .with_writer(std::io::stderr)
                .with_filter(filter_fn(move |metadata| {
                    admits(metadata.level(), stderr_min_level)
                        && output_stream(*metadata.level()) == OutputStream::Stderr
                })),
        )
}

/// The stream one level belongs on.
pub fn output_stream(level: tracing::Level) -> OutputStream {
    match level {
        tracing::Level::WARN | tracing::Level::ERROR => OutputStream::Stderr,
        _ => OutputStream::Stdout,
    }
}

/// Whether an event is at least as severe as the configured minimum. The
/// minimum comes from the environment once and is carried by the filter, so no
/// line of product code can widen or narrow the gate.
pub fn admits(level: &tracing::Level, min_level: LogLevel) -> bool {
    *level <= min_level.as_tracing_level()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{DEFAULT_LOG_LEVEL, InitOptions, OutputStream, admits, output_stream};
    use crate::clock::{EventClock, FixedClock};
    use crate::level::LogLevel;

    #[test]
    fn the_default_gate_is_info() {
        assert_eq!(DEFAULT_LOG_LEVEL, LogLevel::Info);
        assert!(admits(&tracing::Level::INFO, DEFAULT_LOG_LEVEL));
        assert!(admits(&tracing::Level::ERROR, DEFAULT_LOG_LEVEL));
        assert!(!admits(&tracing::Level::DEBUG, DEFAULT_LOG_LEVEL));
    }

    #[test]
    fn the_gate_admits_a_level_and_everything_more_severe() {
        for min_level in LogLevel::ALL {
            for (level, admitted) in [
                (tracing::Level::DEBUG, min_level == LogLevel::Debug),
                (tracing::Level::INFO, min_level <= LogLevel::Info),
                (tracing::Level::WARN, min_level <= LogLevel::Warn),
                (tracing::Level::ERROR, min_level <= LogLevel::Error),
            ] {
                assert_eq!(
                    admits(&level, min_level),
                    admitted,
                    "min {min_level}, level {level}"
                );
            }
        }
    }

    #[test]
    fn warn_and_error_are_the_stderr_channel_and_nothing_else_is() {
        assert_eq!(output_stream(tracing::Level::DEBUG), OutputStream::Stdout);
        assert_eq!(output_stream(tracing::Level::INFO), OutputStream::Stdout);
        assert_eq!(output_stream(tracing::Level::WARN), OutputStream::Stderr);
        assert_eq!(output_stream(tracing::Level::ERROR), OutputStream::Stderr);
    }

    #[test]
    fn the_options_carry_the_gate_and_the_clock_the_host_chose() {
        #[derive(Debug)]
        struct Marker;
        impl EventClock for Marker {
            fn now_epoch_ms(&self) -> i64 {
                7
            }
            fn mono_ns(&self) -> u64 {
                9
            }
        }
        let options = InitOptions::new(Arc::new(Marker))
            .with_min_level(LogLevel::Debug)
            .with_diag_enabled(true);
        assert_eq!(options.min_level, LogLevel::Debug);
        assert!(options.diag_enabled);
        assert_eq!(options.clock.now_epoch_ms(), 7);
        assert_eq!(options.clock.mono_ns(), 9);
        assert_eq!(
            InitOptions::new(Arc::new(FixedClock::new(1, 2))).min_level,
            DEFAULT_LOG_LEVEL,
            "a bare options value is the quiet default"
        );
    }
}
