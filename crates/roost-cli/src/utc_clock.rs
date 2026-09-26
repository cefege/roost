//! Epoch milliseconds to the two UTC stamps `roost doctor` prints, and nothing
//! else. Called by doctor/digest_render.rs and doctor/session_timeline.rs.
//! Depends on no crate: a timezone-free date needs Howard Hinnant's
//! civil-from-days and a fixed table, and pulling a date library in for two
//! format strings would put a second calendar implementation in the binary.
//!
//! The stamps are UTC because a log line's own `ts` is epoch milliseconds, so
//! the only defensible rendering is the one every reader's clock agrees with.
//! A local-time stamp would make two operators in two zones read the same
//! window as two different windows, and this digest exists to be compared.

/// Milliseconds in one day. Every day-boundary computation here is a
/// division by this, and a wrong constant shifts every stamp by hours.
const MS_PER_DAY: i64 = 86_400_000;

/// `1970-01-01 00:00` through `9999-12-31 23:59`, the width `toISOString()`
/// sliced to 16 characters in the TypeScript this replaces. Timestamps outside
/// it cannot come from a log line, but the formatter stays total rather than
/// panicking on one.
pub fn format_utc_minute(epoch_ms: i64) -> String {
    let (year, month, day, _, _, _) = civil_parts(epoch_ms);
    let clock = hour_minute(epoch_ms);
    format!("{year:04}-{month:02}-{day:02} {clock}")
}

/// `HH:MM:SS.mmm` in UTC — the timeline column, where sub-second ordering is
/// the point: two events in the same second are separated by `mono_ns` in the
/// log and collapsing them here would hide the reordering it recorded.
pub fn format_utc_millisecond_time(epoch_ms: i64) -> String {
    let millis_of_day = epoch_ms.rem_euclid(MS_PER_DAY);
    let second = millis_of_day / 1000;
    let millis = millis_of_day % 1000;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        second / 3600,
        (second / 60) % 60,
        second % 60,
        millis
    )
}

fn hour_minute(epoch_ms: i64) -> String {
    let second = epoch_ms.rem_euclid(MS_PER_DAY) / 1000;
    format!("{:02}:{:02}", second / 3600, (second / 60) % 60)
}

type CivilParts = (i64, u32, u32, u32, u32, u32);

/// (year, month, day, hour, minute, second) in UTC.
fn civil_parts(epoch_ms: i64) -> CivilParts {
    let days = epoch_ms.div_euclid(MS_PER_DAY);
    let second_of_day = epoch_ms.rem_euclid(MS_PER_DAY) / 1000;
    let (year, month, day) = civil_from_days(days);
    (
        year,
        month,
        day,
        (second_of_day / 3600) as u32,
        ((second_of_day / 60) % 60) as u32,
        (second_of_day % 60) as u32,
    )
}

/// Hinnant's `civil_from_days`, which shifts the epoch to 0000-03-01 so that
/// the leap day lands at the end of a 400-year era and the month lengths
/// alternate without a table. `days` is days since 1970-01-01 and may be
/// negative; every division here truncates toward zero, which is why the era
/// term is adjusted rather than floored.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_position + 2) / 5 + 1) as u32;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    } as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::{format_utc_millisecond_time, format_utc_minute};

    #[test]
    fn formats_the_epoch_itself() {
        assert_eq!(format_utc_minute(0), "1970-01-01 00:00");
    }

    #[test]
    fn formats_a_leap_day() {
        // 2024-02-29T12:34:56.789Z — a leap day is where a naive 30-day
        // month table drifts, and 2024 is a leap year in the 400-year era.
        let stamp = 1_709_210_096_789;
        assert_eq!(format_utc_minute(stamp), "2024-02-29 12:34");
        assert_eq!(format_utc_millisecond_time(stamp), "12:34:56.789");
    }

    #[test]
    fn pads_every_field_to_two_digits() {
        assert_eq!(format_utc_minute(1_000_000_000_000), "2001-09-09 01:46");
    }
}
