//! The one clock this crate is allowed to have, behind a trait so a test can
//! pin `ts` and `mono_ns` and so no log call reaches for a wall clock the
//! caller cannot see. `ts` is the wall clock in epoch milliseconds; `mono_ns`
//! is a per-process tiebreak for when two lines land in the same millisecond.

use std::sync::LazyLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// Wall and monotonic time for the log line and for `mono_ns`. Implemented by
/// [`SystemClock`] in production and by a fixed clock in tests.
pub trait EventClock: Send + Sync + std::fmt::Debug {
    /// Milliseconds since the Unix epoch. Goes on the line as `ts`.
    fn now_epoch_ms(&self) -> i64;
    /// A monotonically non-decreasing nanosecond counter for this process.
    /// Goes on diag and signal records as `mono_ns`; a process with no
    /// monotonic source reports `0`, which is also the TypeScript fallback.
    fn mono_ns(&self) -> u64;
}

/// The process clock. The only `SystemTime::now()` in the crate lives here,
/// and it is reached only through the clock the host passed to `init`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

/// The monotonic origin of this process, fixed the first time it is read.
static MONO_ORIGIN: LazyLock<Instant> = LazyLock::new(Instant::now);

impl EventClock for SystemClock {
    fn now_epoch_ms(&self) -> i64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(since_epoch) => i64::try_from(since_epoch.as_millis()).unwrap_or(i64::MAX),
            // A wall clock before 1970 cannot describe a real event; reporting
            // 0 keeps the line parseable rather than dropping the event.
            Err(_) => 0,
        }
    }

    fn mono_ns(&self) -> u64 {
        u64::try_from(MONO_ORIGIN.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct FixedClock {
    pub epoch_ms: i64,
    pub mono: u64,
}

#[cfg(test)]
impl FixedClock {
    pub const fn new(epoch_ms: i64, mono: u64) -> Self {
        Self { epoch_ms, mono }
    }
}

#[cfg(test)]
impl EventClock for FixedClock {
    fn now_epoch_ms(&self) -> i64 {
        self.epoch_ms
    }

    fn mono_ns(&self) -> u64 {
        self.mono
    }
}

#[cfg(test)]
mod tests {
    use super::{EventClock, FixedClock, SystemClock};

    #[test]
    fn the_system_clock_reports_a_plausible_epoch_and_a_rising_mono() {
        let clock = SystemClock;
        let epoch_ms = clock.now_epoch_ms();
        // Later than 2020-01-01, so a seconds/millis mix-up cannot pass.
        assert!(epoch_ms > 1_577_836_800_000, "epoch_ms was {epoch_ms}");
        let first = clock.mono_ns();
        assert!(clock.mono_ns() >= first);
    }

    #[test]
    fn a_fixed_clock_reports_exactly_what_the_test_asked_for() {
        let clock = FixedClock::new(1_700_000_000_123, 4_242);
        assert_eq!(clock.now_epoch_ms(), 1_700_000_000_123);
        assert_eq!(clock.mono_ns(), 4_242);
    }
}
