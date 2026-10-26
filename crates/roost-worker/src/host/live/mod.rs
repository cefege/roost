//! The live host and process sampler: this machine's CPU, memory, root disk,
//! uptime and top processes as plain Rust values, for the live-metrics link to
//! project onto the wire. Linux reads `/proc` (`linux`); macOS and Windows read
//! `sysinfo` (`sysinfo_source`); the root disk is `sysinfo` everywhere (`disk`).
//! Never fails: an unreadable source reads as zero, and nothing is logged per sample.

mod disk;
// Off Linux the `/proc` parsers are reached only from tests.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
mod linux;
#[cfg(not(target_os = "linux"))]
mod sysinfo_source;

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use disk::RootDisk;

#[cfg(target_os = "linux")]
type HostSource = linux::ProcFsSampler;
#[cfg(not(target_os = "linux"))]
type HostSource = sysinfo_source::SysinfoSource;

/// One pass of a host source. The sampler adds the disk reading and the
/// first-sample rule; the sources never see either.
#[derive(Debug)]
struct HostReading {
    cpu_pct: f64,
    mem_total_bytes: u64,
    mem_available_bytes: u64,
    uptime_secs: u64,
    processes: Vec<LiveProcessSample>,
}

/// How many processes are kept by CPU and how many by resident memory. The
/// snapshot carries their union, so it can hold up to twice this many rows.
pub const TOP_PROCESS_COUNT: usize = 50;

/// One process in a snapshot. `name` is the command name (comm) only: never
/// argv, environment or working directory.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveProcessSample {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    /// Per core: 100 is one full core, so a multi-threaded process may exceed 100.
    pub cpu_pct: f64,
    pub rss_bytes: u64,
}

/// One sample of the whole host.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveSnapshot {
    /// Busy share of all cores, 0–100.
    pub cpu_pct: f64,
    pub mem_used_bytes: u64,
    pub mem_total_bytes: u64,
    pub disk_used_bytes: u64,
    pub disk_total_bytes: u64,
    pub cpu_count: u32,
    pub uptime_secs: u64,
    /// Every process on the host, not only the ones in `processes`.
    pub process_count: u32,
    /// [`select_top_processes`] of every process: the top [`TOP_PROCESS_COUNT`]
    /// by CPU united with the top [`TOP_PROCESS_COUNT`] by resident memory,
    /// sorted by CPU descending then pid.
    pub processes: Vec<LiveProcessSample>,
    pub sampled_at_ms: u64,
}

/// The stateful live sampler. CPU figures are differences between two readings,
/// so the first [`LiveSampler::sample`] after [`LiveSampler::new`] reports 0 for
/// every CPU figure: the host's, and each process's.
#[derive(Debug)]
pub struct LiveSampler {
    host: HostSource,
    disk: RootDisk,
    cpu_count: u32,
    primed: bool,
}

impl LiveSampler {
    #[must_use]
    pub fn new() -> Self {
        Self {
            host: HostSource::new(),
            disk: RootDisk::new(),
            cpu_count: cpu_count(),
            primed: false,
        }
    }

    /// One sample of the host. Blocking only for the duration of the `/proc`
    /// or sysinfo reads; no sleep is taken to build a CPU difference.
    pub fn sample(&mut self) -> LiveSnapshot {
        let mut host = self.host.sample();
        if !self.primed {
            // sysinfo documents its first CPU refresh as inaccurate, and `/proc`
            // has no earlier reading to difference against. The first sample
            // reports no CPU whichever source produced it.
            self.primed = true;
            host.cpu_pct = 0.0;
            for process in &mut host.processes {
                process.cpu_pct = 0.0;
            }
        }
        let (disk_used_bytes, disk_total_bytes) = self.disk.sample();
        let (processes, process_count) = select_top_processes(host.processes, TOP_PROCESS_COUNT);
        LiveSnapshot {
            cpu_pct: host.cpu_pct,
            mem_used_bytes: host
                .mem_total_bytes
                .saturating_sub(host.mem_available_bytes),
            mem_total_bytes: host.mem_total_bytes,
            disk_used_bytes,
            disk_total_bytes,
            cpu_count: self.cpu_count,
            uptime_secs: host.uptime_secs,
            process_count,
            processes,
            sampled_at_ms: now_ms(),
        }
    }
}

impl Default for LiveSampler {
    fn default() -> Self {
        Self::new()
    }
}

/// The processes to show: the top `top_n` by CPU united with the top `top_n` by
/// resident memory, deduplicated by pid, sorted by CPU descending then pid
/// ascending. Also returns how many processes were offered, which is the host's
/// process count. Ties are broken by pid so the output is deterministic.
#[must_use]
pub fn select_top_processes(
    all: Vec<LiveProcessSample>,
    top_n: usize,
) -> (Vec<LiveProcessSample>, u32) {
    let total = u32::try_from(all.len()).unwrap_or(u32::MAX);
    let mut by_cpu: Vec<usize> = (0..all.len()).collect();
    by_cpu.sort_by(|&a, &b| cpu_order(&all[a], &all[b]));
    let mut by_rss: Vec<usize> = (0..all.len()).collect();
    by_rss.sort_by(|&a, &b| rss_order(&all[a], &all[b]));

    let mut keep = vec![false; all.len()];
    let mut seen_pids = HashSet::new();
    for &index in by_cpu.iter().take(top_n).chain(by_rss.iter().take(top_n)) {
        if seen_pids.insert(all[index].pid) {
            keep[index] = true;
        }
    }

    let mut selected: Vec<LiveProcessSample> = all
        .into_iter()
        .zip(keep)
        .filter_map(|(process, kept)| kept.then_some(process))
        .collect();
    selected.sort_by(cpu_order);
    (selected, total)
}

fn cpu_order(a: &LiveProcessSample, b: &LiveProcessSample) -> std::cmp::Ordering {
    b.cpu_pct.total_cmp(&a.cpu_pct).then(a.pid.cmp(&b.pid))
}

fn rss_order(a: &LiveProcessSample, b: &LiveProcessSample) -> std::cmp::Ordering {
    b.rss_bytes.cmp(&a.rss_bytes).then(a.pid.cmp(&b.pid))
}

/// The logical CPU count, `1` when the platform cannot say.
fn cpu_count() -> u32 {
    std::thread::available_parallelism()
        .map_or(1, |count| u32::try_from(count.get()).unwrap_or(u32::MAX))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(pid: u32, cpu_pct: f64, rss_bytes: u64) -> LiveProcessSample {
        LiveProcessSample {
            pid,
            ppid: 1,
            name: format!("proc-{pid}"),
            cpu_pct,
            rss_bytes,
        }
    }

    #[test]
    fn selection_is_the_deduplicated_union_of_top_cpu_and_top_rss() {
        let all = vec![
            process(10, 90.0, 100),
            process(11, 80.0, 850),
            process(12, 0.5, 900),
            process(13, 0.1, 800),
            process(14, 50.0, 10),
        ];
        let (selected, total) = select_top_processes(all, 2);
        // Top CPU: 10, 11. Top RSS: 12, 11. Pid 11 is in both and appears once.
        let pids: Vec<u32> = selected.iter().map(|process| process.pid).collect();
        assert_eq!(pids, vec![10, 11, 12]);
        assert_eq!(total, 5);
    }

    #[test]
    fn selection_orders_by_cpu_then_pid_and_keeps_everything_when_top_n_is_large() {
        let all = vec![
            process(30, 0.0, 1),
            process(20, 0.0, 3),
            process(40, 5.0, 2),
        ];
        let (selected, total) = select_top_processes(all, 100);
        let pids: Vec<u32> = selected.iter().map(|process| process.pid).collect();
        assert_eq!(pids, vec![40, 20, 30]);
        assert_eq!(total, 3);
    }

    #[test]
    fn selection_of_nothing_is_nothing() {
        let (selected, total) = select_top_processes(Vec::new(), TOP_PROCESS_COUNT);
        assert!(selected.is_empty());
        assert_eq!(total, 0);
    }

    /// Runs against the live machine, which is safe: the assertions hold for
    /// whatever processes exist, and nothing here names a particular process.
    #[test]
    fn first_sample_after_new_reports_no_cpu() {
        let mut sampler = LiveSampler::new();
        let first = sampler.sample();
        assert_eq!(first.cpu_pct, 0.0);
        assert!(first.processes.iter().all(|process| process.cpu_pct == 0.0));
        assert!(first.cpu_count >= 1);
        assert!(first.processes.len() <= first.process_count as usize);
        assert!(first.processes.len() <= 2 * TOP_PROCESS_COUNT);
    }

    #[test]
    fn second_sample_keeps_host_cpu_within_bounds() {
        let mut sampler = LiveSampler::new();
        let _first = sampler.sample();
        let second = sampler.sample();
        assert!((0.0..=100.0).contains(&second.cpu_pct));
        assert!(second.mem_used_bytes <= second.mem_total_bytes);
    }
}
