//! Pure readings for the Fleet page: when a sample stops describing its
//! machine, how long a machine has been up, and the order of its process table.
//! No rendering lives here, so the thresholds and orderings are unit-tested on
//! the native target.

use std::cmp::Ordering;

use roost_protocol::wire::live_metrics::{LiveHostSnapshot, LiveProcess};

/// A sample older than this no longer describes its machine. The card then
/// shows the sample's age instead of readings it would present as current.
pub const STALE_AFTER_MS: i64 = 6_000;

/// Whether a sample taken at `sampled_at_ms` is too old to show as live. A
/// sample stamped after `now_ms` (clock skew between worker and browser) is
/// fresh, not stale.
pub fn is_stale(now_ms: i64, sampled_at_ms: u64) -> bool {
    sample_age_ms(now_ms, sampled_at_ms) > STALE_AFTER_MS
}

/// Whole seconds since the sample, never negative.
pub fn sample_age_secs(now_ms: i64, sampled_at_ms: u64) -> u64 {
    u64::try_from(sample_age_ms(now_ms, sampled_at_ms)).unwrap_or(0) / 1_000
}

fn sample_age_ms(now_ms: i64, sampled_at_ms: u64) -> i64 {
    let sampled = i64::try_from(sampled_at_ms).unwrap_or(i64::MAX);
    now_ms.saturating_sub(sampled).max(0)
}

/// What a machine's card can honestly show about its host right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingState {
    /// Not reachable: no reading is shown and none is subscribed.
    Offline,
    /// Online and subscribed, but no sample has arrived yet.
    Awaiting,
    /// The last sample is older than [`STALE_AFTER_MS`]; `age_secs` says how old.
    Stale { age_secs: u64 },
    /// A fresh sample: the card shows its readings.
    Live,
}

/// The reading state for one registered machine.
pub fn reading_state(
    online: bool,
    snapshot: Option<&LiveHostSnapshot>,
    now_ms: i64,
) -> ReadingState {
    match (online, snapshot) {
        (false, _) => ReadingState::Offline,
        (true, None) => ReadingState::Awaiting,
        (true, Some(sample)) if is_stale(now_ms, sample.sampled_at_ms) => ReadingState::Stale {
            age_secs: sample_age_secs(now_ms, sample.sampled_at_ms),
        },
        (true, Some(_)) => ReadingState::Live,
    }
}

/// A compact uptime that keeps the two largest non-zero units: `3d 4h`,
/// `2h 5m`, `7m 3s`, `42s`.
pub fn format_uptime(total_secs: u64) -> String {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let days = total_secs / DAY;
    let hours = (total_secs % DAY) / HOUR;
    let minutes = (total_secs % HOUR) / MINUTE;
    let seconds = total_secs % MINUTE;
    if days > 0 {
        join_units(days, "d", hours, "h")
    } else if hours > 0 {
        join_units(hours, "h", minutes, "m")
    } else if minutes > 0 {
        join_units(minutes, "m", seconds, "s")
    } else {
        format!("{seconds}s")
    }
}

fn join_units(major: u64, major_unit: &str, minor: u64, minor_unit: &str) -> String {
    if minor == 0 {
        format!("{major}{major_unit}")
    } else {
        format!("{major}{major_unit} {minor}{minor_unit}")
    }
}

/// The columns the process table sorts by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessSortColumn {
    Pid,
    Name,
    Cpu,
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessSort {
    pub column: ProcessSortColumn,
    pub direction: SortDirection,
}

impl Default for ProcessSort {
    /// Busiest first: what a reader opening the table is looking for.
    fn default() -> Self {
        Self {
            column: ProcessSortColumn::Cpu,
            direction: SortDirection::Descending,
        }
    }
}

/// The sort after a header click. The same column flips direction; a new column
/// starts in its natural direction.
pub fn next_sort(current: ProcessSort, clicked: ProcessSortColumn) -> ProcessSort {
    if current.column == clicked {
        let direction = match current.direction {
            SortDirection::Ascending => SortDirection::Descending,
            SortDirection::Descending => SortDirection::Ascending,
        };
        ProcessSort {
            column: clicked,
            direction,
        }
    } else {
        ProcessSort {
            column: clicked,
            direction: natural_direction(clicked),
        }
    }
}

fn natural_direction(column: ProcessSortColumn) -> SortDirection {
    match column {
        ProcessSortColumn::Name => SortDirection::Ascending,
        ProcessSortColumn::Pid | ProcessSortColumn::Cpu | ProcessSortColumn::Memory => {
            SortDirection::Descending
        }
    }
}

/// Orders processes by `sort`. The pid breaks every tie, ascending, so equal
/// rows keep their places between renders.
pub fn sort_processes(processes: &mut [LiveProcess], sort: ProcessSort) {
    processes.sort_by(|left, right| {
        let by_column = match sort.column {
            ProcessSortColumn::Pid => left.pid.cmp(&right.pid),
            ProcessSortColumn::Name => compare_names(&left.name, &right.name),
            ProcessSortColumn::Cpu => left.cpu_pct.total_cmp(&right.cpu_pct),
            ProcessSortColumn::Memory => left.rss_bytes.cmp(&right.rss_bytes),
        };
        let ordered = match sort.direction {
            SortDirection::Ascending => by_column,
            SortDirection::Descending => by_column.reverse(),
        };
        ordered.then(left.pid.cmp(&right.pid))
    });
}

/// Case-insensitive for ASCII and allocation-free: each byte compares lowercased.
fn compare_names(left: &str, right: &str) -> Ordering {
    left.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(right.bytes().map(|byte| byte.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, name: &str, cpu_pct: f64, rss_bytes: u64) -> LiveProcess {
        LiveProcess {
            pid,
            ppid: 1,
            name: name.to_owned(),
            cpu_pct,
            rss_bytes,
        }
    }

    fn pids(processes: &[LiveProcess]) -> Vec<u32> {
        processes.iter().map(|process| process.pid).collect()
    }

    fn sample(sampled_at_ms: u64) -> LiveHostSnapshot {
        LiveHostSnapshot {
            cpu_pct: 0.0,
            mem_used_bytes: 0,
            mem_total_bytes: 0,
            disk_used_bytes: 0,
            disk_total_bytes: 0,
            cpu_count: 1,
            uptime_secs: 0,
            process_count: 0,
            processes: Vec::new(),
            sampled_at_ms,
        }
    }

    #[test]
    fn sort_default_is_busiest_first_with_pid_tie_break() {
        let mut processes = vec![
            process(30, "c", 5.0, 1),
            process(20, "b", 9.0, 1),
            process(10, "a", 5.0, 1),
        ];
        sort_processes(&mut processes, ProcessSort::default());
        assert_eq!(pids(&processes), vec![20, 10, 30]);
    }

    #[test]
    fn sort_by_name_ignores_ascii_case() {
        let mut processes = vec![
            process(1, "beta", 0.0, 0),
            process(2, "Alpha", 0.0, 0),
            process(3, "gamma", 0.0, 0),
        ];
        let sort = ProcessSort {
            column: ProcessSortColumn::Name,
            direction: SortDirection::Ascending,
        };
        sort_processes(&mut processes, sort);
        assert_eq!(pids(&processes), vec![2, 1, 3]);
    }

    #[test]
    fn sort_by_memory_descending_keeps_pid_order_on_equal_memory() {
        let mut processes = vec![
            process(7, "a", 0.0, 100),
            process(4, "b", 0.0, 300),
            process(9, "c", 0.0, 300),
        ];
        let sort = ProcessSort {
            column: ProcessSortColumn::Memory,
            direction: SortDirection::Descending,
        };
        sort_processes(&mut processes, sort);
        assert_eq!(pids(&processes), vec![4, 9, 7]);
    }

    #[test]
    fn next_sort_flips_the_same_column_and_starts_new_columns_naturally() {
        let start = ProcessSort::default();
        let flipped = next_sort(start, ProcessSortColumn::Cpu);
        assert_eq!(flipped.direction, SortDirection::Ascending);
        assert_eq!(next_sort(flipped, ProcessSortColumn::Cpu), start);

        let name = next_sort(start, ProcessSortColumn::Name);
        assert_eq!(
            name,
            ProcessSort {
                column: ProcessSortColumn::Name,
                direction: SortDirection::Ascending,
            }
        );
        let memory = next_sort(name, ProcessSortColumn::Memory);
        assert_eq!(memory.direction, SortDirection::Descending);
    }

    #[test]
    fn staleness_begins_only_after_the_threshold() {
        let now_ms = 10_000;
        assert!(!is_stale(now_ms, 4_000), "exactly the threshold is fresh");
        assert!(is_stale(now_ms, 3_999));
        assert!(!is_stale(now_ms, 10_500), "a future sample is fresh");
    }

    #[test]
    fn sample_age_is_whole_seconds_and_never_negative() {
        assert_eq!(sample_age_secs(10_000, 4_500), 5);
        assert_eq!(sample_age_secs(10_000, 20_000), 0);
    }

    #[test]
    fn reading_state_follows_online_then_sample_then_age() {
        let now_ms = 60_000;
        assert_eq!(
            reading_state(false, Some(&sample(now_ms as u64)), now_ms),
            ReadingState::Offline
        );
        assert_eq!(reading_state(true, None, now_ms), ReadingState::Awaiting);
        assert_eq!(
            reading_state(true, Some(&sample(59_000)), now_ms),
            ReadingState::Live
        );
        assert_eq!(
            reading_state(true, Some(&sample(40_000)), now_ms),
            ReadingState::Stale { age_secs: 20 }
        );
    }

    #[test]
    fn uptime_keeps_the_two_largest_non_zero_units() {
        assert_eq!(format_uptime(0), "0s");
        assert_eq!(format_uptime(59), "59s");
        assert_eq!(format_uptime(60), "1m");
        assert_eq!(format_uptime(61), "1m 1s");
        assert_eq!(format_uptime(3_600), "1h");
        assert_eq!(format_uptime(3_660), "1h 1m");
        assert_eq!(format_uptime(86_400), "1d");
        assert_eq!(format_uptime(90_000), "1d 1h");
        assert_eq!(format_uptime(2 * 86_400 + 3 * 3_600 + 5), "2d 3h");
    }
}
