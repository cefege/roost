//! The wording the settings panes share: how a timestamp reads, and how a long
//! identifier fits a row.
//!
//! Ports the `relativeTime`/`formatAge`/`shortFp`/`fmtTs` helpers the v2 panes
//! each carried a copy of. One copy here, because a fleet pane that says "3m
//! ago" for a machine and "3 minutes ago" for an attachment is two spellings of
//! one fact.

/// A whole-second elapsed time, in v2's wording.
pub fn relative_time(now_ms: u64, then_ms: u64) -> String {
    let elapsed_ms = now_ms.saturating_sub(then_ms);
    let seconds = elapsed_ms / 1_000;
    if seconds < 5 {
        return "just now".to_owned();
    }
    if seconds < 60 {
        return format!("{seconds}s ago");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    format!("{}h ago", minutes / 60)
}

/// An attachment's age, in the coarser wording a file list uses.
pub fn format_age(now_ms: u64, then_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(then_ms).div_ceil(60_000);
    if minutes < 1 {
        return "just now".to_owned();
    }
    if minutes < 60 {
        return format!("{minutes} min ago");
    }
    let hours = minutes.div_ceil(60);
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours.div_ceil(24))
}

/// How long an attachment has left before the sweep takes it.
pub fn expiry_label(now_ms: u64, mtime_ms: u64) -> &'static str {
    /// The sweep window, from `AttachmentsPane.tsx`.
    const TTL_MS: u64 = 24 * 60 * 60 * 1_000;
    /// Inside this the row warns rather than reporting a whole day.
    const WARN_MS: u64 = 60 * 60 * 1_000;
    match mtime_ms.saturating_add(TTL_MS).saturating_sub(now_ms) {
        0 => "Expired",
        remaining if remaining < WARN_MS => "Under 1h left",
        _ => "Under 24h left",
    }
}

/// Whether an attachment is past its sweep, or close enough to say so.
pub fn is_expiring(now_ms: u64, mtime_ms: u64) -> bool {
    expiry_label(now_ms, mtime_ms) != "Under 24h left"
}

/// A fingerprint or trace id clipped to its ends, the way v2's `shortFp` did.
pub fn short_identifier(value: &str, keep: usize) -> String {
    if value.chars().count() <= keep {
        return value.to_owned();
    }
    let head: String = value.chars().take(keep.saturating_div(2)).collect();
    let tail: String = value
        .chars()
        .skip(value.chars().count().saturating_sub(keep.saturating_div(2)))
        .collect();
    format!("{head}…{tail}")
}

/// The wall-clock stamp an audit row and a device row both print.
pub fn format_timestamp(seconds: u64) -> String {
    // `civil_from_days` counts from 1970-01-01 itself, so the day count here is
    // plain days-since-epoch; shifting it here as well put every stamp in 3939.
    let days = (seconds / 86_400) as i64;
    let time_of_day = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        time_of_day / 3_600,
        (time_of_day % 3_600) / 60,
        time_of_day % 60
    )
}

/// Howard Hinnant's civil-from-days, valid for the whole `i64` day range.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = u32::try_from(day_of_year - (153 * month_index + 2) / 5 + 1).unwrap_or(1);
    let month = u32::try_from(if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    })
    .unwrap_or(1);
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_time_reads_in_v2_words() {
        assert_eq!(relative_time(10_000, 9_000), "just now");
        assert_eq!(relative_time(20_000, 10_000), "10s ago");
        assert_eq!(relative_time(200_000, 10_000), "3m ago");
        // Two hours exactly. 7_200_001ms is one second past the hour and still
        // reads "2h ago", because v2 floors rather than rounds.
        assert_eq!(relative_time(7_200_000, 0), "2h ago");
        assert_eq!(relative_time(7_200_001, 0), "2h ago");
    }

    #[test]
    fn a_future_timestamp_does_not_wrap() {
        assert_eq!(relative_time(1_000, 9_000), "just now");
    }

    #[test]
    fn expiry_marks_the_sweep_window() {
        let now = 1_000_000_000_u64;
        // 23h59m old leaves a minute, which is inside the warn band; 23h old
        // leaves exactly an hour, which is not — the boundary is strict, as
        // v2's `remaining < WARN_THRESHOLD_MS` is.
        assert_eq!(
            expiry_label(now, now - 23 * 3_600_000 - 60_000),
            "Under 1h left"
        );
        assert_eq!(expiry_label(now, now - 23 * 3_600_000), "Under 24h left");
        assert_eq!(expiry_label(now, now - 25 * 3_600_000), "Expired");
        assert!(is_expiring(now, now - 23 * 3_600_000 - 60_000));
        assert!(!is_expiring(now, now));
    }

    #[test]
    fn timestamps_render_as_utc() {
        assert_eq!(format_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
    }

    #[test]
    fn a_short_identifier_is_left_alone() {
        assert_eq!(short_identifier("abc", 8), "abc");
        assert_eq!(short_identifier("0123456789", 8), "0123…6789");
    }
}
