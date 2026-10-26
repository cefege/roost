//! Linux host and process readings from `/proc`: the aggregate CPU row, each
//! process's jiffies and resident set, memory and uptime. The parsers are plain
//! functions over text so every OS compiles and tests them; only
//! [`ProcFsSampler`] touches the filesystem, and [`super::LiveSampler`] uses it on Linux alone.
//! Forbids `unsafe` and `libc`: the clock tick is never read, because CPU
//! percentages are ratios of jiffies and the tick cancels out.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::{HostReading, LiveProcessSample};

/// Cumulative jiffies of the aggregate `cpu` row of `/proc/stat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CpuJiffies {
    /// Idle plus iowait: the CPU was not running work.
    idle: u64,
    /// Every field of the row, summed across all cores.
    total: u64,
}

/// What one `/proc/<pid>/stat` row says about a process.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcStat {
    ppid: u32,
    name: String,
    /// utime + stime.
    jiffies: u64,
}

/// Reads `/proc` on every sample. Keeps the previous aggregate and per-process
/// jiffies because CPU usage is a difference between two readings.
#[derive(Debug)]
pub(super) struct ProcFsSampler {
    cpu_count: u32,
    previous_cpu: Option<CpuJiffies>,
    previous_process_jiffies: HashMap<u32, u64>,
}

impl ProcFsSampler {
    pub(super) fn new() -> Self {
        Self {
            cpu_count: super::cpu_count(),
            previous_cpu: None,
            previous_process_jiffies: HashMap::new(),
        }
    }

    /// Unreadable sources read as zero, and a process whose rows fail to parse
    /// is left out rather than reported with guessed fields.
    pub(super) fn sample(&mut self) -> HostReading {
        let cpu_now = read_text("/proc/stat").as_deref().and_then(parse_stat_cpu);
        let (cpu_pct, total_delta) = match (cpu_now, self.previous_cpu) {
            (Some(now), Some(previous)) => (
                host_cpu_pct(previous, now),
                i128::from(now.total) - i128::from(previous.total),
            ),
            _ => (0.0, 0),
        };
        self.previous_cpu = cpu_now.or(self.previous_cpu);

        let mut processes = Vec::new();
        let mut process_jiffies = HashMap::new();
        for entry in fs::read_dir("/proc").into_iter().flatten().flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<u32>().ok())
            else {
                continue;
            };
            let directory = entry.path();
            let Some(stat) = read_text(directory.join("stat"))
                .as_deref()
                .and_then(parse_pid_stat)
            else {
                continue;
            };
            let rss_bytes = read_text(directory.join("status"))
                .as_deref()
                .map_or(0, parse_status_rss_bytes);
            let previous = self.previous_process_jiffies.get(&pid).copied();
            processes.push(LiveProcessSample {
                pid,
                ppid: stat.ppid,
                name: stat.name,
                cpu_pct: process_cpu_pct(previous, stat.jiffies, total_delta, self.cpu_count),
                rss_bytes,
            });
            process_jiffies.insert(pid, stat.jiffies);
        }
        // Replaced, not merged: a process that exited must not keep its old
        // reading, or a reused pid would difference against an unrelated process.
        self.previous_process_jiffies = process_jiffies;

        let (mem_total_bytes, mem_available_bytes) = read_text("/proc/meminfo")
            .as_deref()
            .map_or((0, 0), parse_meminfo_bytes);
        let uptime_secs = read_text("/proc/uptime")
            .as_deref()
            .map_or(0, parse_uptime_secs);
        HostReading {
            cpu_pct,
            mem_total_bytes,
            mem_available_bytes,
            uptime_secs,
            processes,
        }
    }
}

fn read_text(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// The aggregate `cpu` row of `/proc/stat`. `None` when the row is missing or
/// any field fails to parse. The per-core `cpuN` rows never match: the prefix
/// includes the space that follows the aggregate's name.
fn parse_stat_cpu(stat: &str) -> Option<CpuJiffies> {
    let fields = stat
        .lines()
        .find_map(|line| line.strip_prefix("cpu "))?
        .split_whitespace()
        .map(|field| field.parse::<u64>().ok())
        .collect::<Option<Vec<u64>>>()?;
    // user nice system idle iowait irq softirq steal guest guest_nice
    let idle = fields
        .get(3)?
        .saturating_add(fields.get(4).copied().unwrap_or(0));
    let total = fields
        .iter()
        .fold(0_u64, |sum, field| sum.saturating_add(*field));
    Some(CpuJiffies { idle, total })
}

/// `/proc/<pid>/stat`. The command name sits between the first `(` and the last
/// `)`, because it may hold spaces and parentheses of its own. Fields after the
/// name are numbered as proc(5) numbers them, from 1 with the pid first.
fn parse_pid_stat(stat: &str) -> Option<ProcStat> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let name = stat.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = stat.get(close + 1..)?.split_whitespace().collect();
    // `rest` starts at field 3 (state), so field N is `rest[N - 3]`.
    let field = |number: usize| rest.get(number.checked_sub(3)?).copied();
    let ppid = field(4)?.parse::<u32>().ok()?;
    let utime = field(14)?.parse::<u64>().ok()?;
    let stime = field(15)?.parse::<u64>().ok()?;
    Some(ProcStat {
        ppid,
        name,
        jiffies: utime.saturating_add(stime),
    })
}

/// `VmRSS` of `/proc/<pid>/status`, in bytes. Kernel threads have no such row
/// and read as 0.
fn parse_status_rss_bytes(status: &str) -> u64 {
    kb_row_bytes(status, "VmRSS:").unwrap_or(0)
}

/// `(MemTotal, MemAvailable)` of `/proc/meminfo`, in bytes; a missing row reads as 0.
fn parse_meminfo_bytes(meminfo: &str) -> (u64, u64) {
    (
        kb_row_bytes(meminfo, "MemTotal:").unwrap_or(0),
        kb_row_bytes(meminfo, "MemAvailable:").unwrap_or(0),
    )
}

/// The first value of the row that starts with `key`, read as kB and returned
/// in bytes. Every kB row is `Key:<spaces>N kB`.
fn kb_row_bytes(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|kb| kb.parse::<u64>().ok())
        .map(|kb| kb.saturating_mul(1024))
}

/// Whole seconds from the first field of `/proc/uptime` (`"12345.67 98765.43"`).
/// Parsed as an integer prefix, so no float rounding enters the figure.
fn parse_uptime_secs(uptime: &str) -> u64 {
    uptime
        .split_whitespace()
        .next()
        .and_then(|field| field.split('.').next())
        .and_then(|secs| secs.parse::<u64>().ok())
        .unwrap_or(0)
}

/// Busy share of the aggregate delta, 0–100. Zero when the total did not advance.
fn host_cpu_pct(previous: CpuJiffies, now: CpuJiffies) -> f64 {
    let total_delta = now.total.saturating_sub(previous.total);
    if total_delta == 0 {
        return 0.0;
    }
    let idle_delta = now.idle.saturating_sub(previous.idle).min(total_delta);
    let busy_delta = total_delta - idle_delta;
    (100.0 * busy_delta as f64 / total_delta as f64).clamp(0.0, 100.0)
}

/// One process's CPU as a share of one core: `100 * cpu_count * dproc / dtotal`.
/// `total_delta` sums every core, so multiplying by `cpu_count` turns it back
/// into one core's wall time; a process that keeps one core busy reads 100.
/// Zero for a process with no previous reading (new, or first sample), and for
/// a non-advancing total. Clamped at zero: a reused pid can read lower jiffies.
fn process_cpu_pct(
    previous_jiffies: Option<u64>,
    now_jiffies: u64,
    total_delta: i128,
    cpu_count: u32,
) -> f64 {
    let Some(previous) = previous_jiffies else {
        return 0.0;
    };
    if total_delta <= 0 {
        return 0.0;
    }
    let process_delta = i128::from(now_jiffies) - i128::from(previous);
    let pct = 100.0 * f64::from(cpu_count) * process_delta as f64 / total_delta as f64;
    pct.max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "cpu  100 20 30 800 50 5 5 0 0 0\n\
                        cpu0 50 10 15 400 25 2 2 0 0 0\n\
                        cpu1 50 10 15 400 25 3 3 0 0 0\n\
                        intr 12345\n";

    #[test]
    fn stat_aggregate_row_sums_fields_and_counts_iowait_as_idle() {
        let jiffies = parse_stat_cpu(STAT).expect("aggregate row parses");
        assert_eq!(jiffies.idle, 850);
        assert_eq!(jiffies.total, 1010);
    }

    #[test]
    fn stat_without_an_aggregate_row_or_with_a_bad_field_is_none() {
        assert_eq!(parse_stat_cpu("cpu0 1 2 3 4\n"), None);
        assert_eq!(parse_stat_cpu("cpu  1 2 x 4\n"), None);
        assert_eq!(parse_stat_cpu("cpu  1 2 3\n"), None);
    }

    /// comm holds a space and parentheses; the name must run from the first
    /// `(` to the last `)`, and the numeric fields must still be found after it.
    #[test]
    fn pid_stat_reads_name_with_spaces_and_parens_and_the_right_fields() {
        let stat = "1234 (my (weird) proc) S 1 1234 1234 0 -1 4194304 100 0 0 0 7 3 0 0 20 0 1 0";
        assert_eq!(
            parse_pid_stat(stat),
            Some(ProcStat {
                ppid: 1,
                name: "my (weird) proc".to_string(),
                jiffies: 10,
            })
        );
    }

    #[test]
    fn pid_stat_with_plain_comm_and_truncated_row() {
        let stat = "42 (sshd) S 7 42 42 0 -1 0 0 0 0 0 2 5 0 0 20 0 1 0";
        let parsed = parse_pid_stat(stat).expect("plain comm parses");
        assert_eq!(parsed.name, "sshd");
        assert_eq!(parsed.ppid, 7);
        assert_eq!(parsed.jiffies, 7);
        assert_eq!(parse_pid_stat("42 (sshd) S 7 42"), None);
        assert_eq!(parse_pid_stat("42 sshd S 7"), None);
    }

    #[test]
    fn status_vm_rss_is_kb_converted_to_bytes_and_absent_is_zero() {
        let status = "Name:\tfoo\nVmPeak:\t  9999 kB\nVmRSS:\t    1234 kB\nVmHWM:\t 2000 kB\n";
        assert_eq!(parse_status_rss_bytes(status), 1234 * 1024);
        assert_eq!(parse_status_rss_bytes("Name:\tkworker\n"), 0);
    }

    #[test]
    fn meminfo_total_and_available_in_bytes_and_not_confused_by_prefixes() {
        let meminfo = "MemTotal:       16000000 kB\nMemFree:  1 kB\nMemAvailable:    4000000 kB\n";
        assert_eq!(
            parse_meminfo_bytes(meminfo),
            (16_000_000 * 1024, 4_000_000 * 1024)
        );
        assert_eq!(parse_meminfo_bytes(""), (0, 0));
    }

    #[test]
    fn uptime_is_the_integer_part_of_the_first_field() {
        assert_eq!(parse_uptime_secs("12345.67 98765.43\n"), 12345);
        assert_eq!(parse_uptime_secs("garbage"), 0);
    }

    #[test]
    fn host_cpu_is_busy_share_of_the_aggregate_delta() {
        let previous = CpuJiffies { idle: 100, total: 200 };
        let now = CpuJiffies { idle: 150, total: 400 };
        assert!((host_cpu_pct(previous, now) - 75.0).abs() < 1e-9);
        assert_eq!(host_cpu_pct(now, now), 0.0);
        let backwards = CpuJiffies { idle: 0, total: 100 };
        assert_eq!(host_cpu_pct(now, backwards), 0.0);
    }

    #[test]
    fn process_cpu_is_per_core_and_clamped_and_zero_without_history() {
        // 50 jiffies of 1000 total across 4 cores: 100 * 4 * 50 / 1000 = 20.
        assert!((process_cpu_pct(Some(100), 150, 1000, 4) - 20.0).abs() < 1e-9);
        // A process that was not seen last sample has no delta.
        assert_eq!(process_cpu_pct(None, 150, 1000, 4), 0.0);
        // A total that did not advance, or went backwards, reads as zero load.
        assert_eq!(process_cpu_pct(Some(100), 150, 0, 4), 0.0);
        assert_eq!(process_cpu_pct(Some(100), 150, -5, 4), 0.0);
        // Lower jiffies on a reused pid clamp to zero rather than going negative.
        assert_eq!(process_cpu_pct(Some(500), 150, 1000, 4), 0.0);
    }
}
