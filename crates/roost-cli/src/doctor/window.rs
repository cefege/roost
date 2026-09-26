//! The `--since` window: a number and a unit, parsed once and used by every
//! filter in the digest. Called by doctor/mod.rs. Pure, so a bad window is a
//! refusal before a single log file is opened.
//!
//! The unit is mandatory on purpose. `--since 24` reads as 24 of what? An
//! unbounded or default-interpreted window on a command whose whole job is to
//! be pasted into a daily review is how a week of logs silently becomes a day
//! of logs, or a day becomes a week of noise nobody reads.

use crate::command_error::{CommandFailure, USAGE_FAILURE};

/// The label a window prints with, and the one `--help` shows. 24 hours is the
/// daily-review default this command was built for: long enough to cover a
/// night, short enough that the digest still fits on one screen.
pub const DEFAULT_WINDOW_LABEL: &str = "24h";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Exactly what the operator typed, echoed into the digest's heading.
    pub label: String,
    pub millis: i64,
}

/// Refuses with exit 2, not 1: the window is an argument, so a malformed one is
/// a usage error, and a cron job that wraps this command should be able to
/// tell "the operator mistyped" from "the logs are alarming".
pub fn parse_window(label: &str) -> Result<Window, CommandFailure> {
    let (digits, unit) = label.split_at(label.len().saturating_sub(1));
    let millis_per_unit = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err(bad_window(label)),
    };
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(bad_window(label));
    }
    let count: i64 = digits.parse().map_err(|_| bad_window(label))?;
    let millis = count
        .checked_mul(millis_per_unit)
        .ok_or_else(|| bad_window(label))?;
    Ok(Window {
        label: label.to_string(),
        millis,
    })
}

fn bad_window(label: &str) -> CommandFailure {
    CommandFailure::new(
        USAGE_FAILURE,
        format!("bad --since \"{label}\" (use e.g. 24h, 7d, 90m)"),
    )
}

#[cfg(test)]
mod tests {
    // A test's `unwrap` is the assertion: it panics on exactly the value the
    // test says must parse, which is the failure this module exists to catch.
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::{DEFAULT_WINDOW_LABEL, parse_window};

    #[test]
    fn every_documented_unit_parses() {
        assert_eq!(parse_window("90m").unwrap().millis, 5_400_000);
        assert_eq!(parse_window("24h").unwrap().millis, 86_400_000);
        assert_eq!(parse_window("7d").unwrap().millis, 604_800_000);
        assert_eq!(parse_window("30s").unwrap().millis, 30_000);
    }

    #[test]
    fn the_label_is_echoed_back_verbatim() {
        assert_eq!(parse_window("7d").unwrap().label, "7d");
        assert_eq!(parse_window(DEFAULT_WINDOW_LABEL).unwrap().label, "24h");
    }

    #[test]
    fn a_bare_number_is_refused_because_the_unit_is_the_point() {
        let error = parse_window("24").unwrap_err();
        assert_eq!(error.code, 2);
    }

    #[test]
    fn a_window_that_would_overflow_is_refused_rather_than_wrapping() {
        // A wrapped negative cutoff would make every log line look like it came
        // from the future, which reads as an empty digest — a silent all-clear.
        assert!(parse_window("99999999999999999999d").is_err());
    }

    #[test]
    fn unknown_units_are_refused() {
        for label in ["24x", "h", "", "-1h", "24 h", "1.5h"] {
            assert!(parse_window(label).is_err(), "{label} was accepted");
        }
    }
}
