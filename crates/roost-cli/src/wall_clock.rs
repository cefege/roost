//! The one wall-clock read in the CLI, in epoch milliseconds. Called by the
//! commands that age something — a worker heartbeat, a keeper reconciliation,
//! a log window — so that every age in every command is measured against the
//! same clock read once per command rather than once per line.
//!
//! A readout that calls the clock inside its renderer is a readout whose two
//! age columns disagree, and one that cannot be handed a fixed instant cannot
//! be asserted at all.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the Unix epoch, or 0 on a host whose clock predates it.
/// A pre-epoch clock makes every age read as enormous, which is a louder lie
/// than a zero, and no log line can carry one.
pub fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::now_ms;

    #[test]
    fn reads_a_plausible_epoch_millisecond_count() {
        // 2020-01-01T00:00:00Z. A zero here would mean the conversion is
        // broken, and a value below this would mean the units are seconds.
        assert!(now_ms() > 1_577_836_800_000);
    }
}
