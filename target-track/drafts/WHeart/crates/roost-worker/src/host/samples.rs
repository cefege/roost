//! The raw counters a heartbeat ships — CPU, memory, disk and interface bytes —
//! read from this host, and the cgroup-v2 throttle probe. Ports v2
//! `apps/worker/src/host/host-sample-linux.ts`; the macOS half is
//! [`super::samples_darwin`] (v2 `host-sample-darwin.ts`). Called by
//! `runtime::heartbeat_metrics`, which owns the rate, the sixty-second cache
//! and the wire shape. Depends on `roost_host::HostPlatform` and `std`.
//!
//! NEVER FAILS. A sampler that returned an error would mean a heartbeat is not
//! sent, so a machine that cannot answer `/proc` or `vm_stat` would vanish from
//! the fleet view rather than report zeros. Every failure here is a zero, and a
//! platform with no sampler (Windows is paused) is all zeros too.

use std::time::Duration;

use roost_host::HostPlatform;
use roost_host::host_memory::{cgroup2_base, read_linux_memory_file};

use super::samples_darwin::DarwinSampler;
use super::tool_path::run_bounded;
use super::{HostSample, NetCounters};

/// v2's `execSync(.., { timeout: 1000 })` bound on every Linux sampler tool.
pub(super) const SAMPLER_TOOL_TIMEOUT: Duration = Duration::from_secs(1);

/// This process's own cgroup's memory throttle counters, when cgroup v2 is
/// present and the unit is actually limited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupPressure {
    pub current_bytes: u64,
    pub high_bytes: u64,
    pub high_events: u64,
}

/// One sampler for one host.
///
/// Stateful because two readings cannot be taken in one pass: Linux CPU
/// percentage is a difference between two `/proc/stat` readings, and the
/// interface to watch comes from a route lookup whose answer is memoized (v2
/// `_prevCpu`, `_primaryIface`). Holding that here keeps it off a module
/// global, where a second sampler and a test would fight over the same slot.
#[derive(Debug)]
pub struct HostSampler {
    platform: HostPlatform,
    previous_cpu: Option<CpuReading>,
    primary_interface: Option<String>,
    darwin: DarwinSampler,
}

impl HostSampler {
    #[must_use]
    pub const fn new(platform: HostPlatform) -> Self {
        Self {
            platform,
            previous_cpu: None,
            primary_interface: None,
            darwin: DarwinSampler::new(),
        }
    }

    /// One sample of this host's raw counters.
    pub fn sample(&mut self) -> HostSample {
        match self.platform {
            HostPlatform::Linux => self.sample_linux(),
            HostPlatform::MacOs => self.darwin.sample(),
            HostPlatform::Windows => HostSample::default(),
        }
    }

    /// v2 `host-sample-linux.ts` `sampleHost`. The first CPU reading after a
    /// worker starts is zero, and that is not a defect: `/proc/stat` holds
    /// jiffies since boot, so a single read says nothing about current load.
    fn sample_linux(&mut self) -> HostSample {
        let cpu_pct = self.sample_linux_cpu_pct();
        let (mem_used_bytes, mem_total_bytes) = sample_linux_memory();
        let (disk_used_bytes, disk_total_bytes) = sample_disk();
        let interface = self.primary_linux_interface().to_string();
        HostSample {
            cpu_pct,
            mem_used_bytes,
            mem_total_bytes,
            disk_used_bytes,
            disk_total_bytes,
            net: sample_linux_net(&interface),
        }
    }

    fn sample_linux_cpu_pct(&mut self) -> f64 {
        let Some(reading) = read_cpu_jiffies() else {
            return 0.0;
        };
        let Some(previous) = self.previous_cpu.replace(reading) else {
            return 0.0;
        };
        // Signed, as v2's plain subtraction is: a counter that went backwards
        // is a non-positive total and reads as zero load.
        let total_delta = reading.total as i128 - previous.total as i128;
        let idle_delta = reading.idle as i128 - previous.idle as i128;
        if total_delta <= 0 {
            return 0.0;
        }
        let busy_pct = 100.0 * (1.0 - idle_delta as f64 / total_delta as f64);
        busy_pct.round().clamp(0.0, 100.0)
    }

    /// The interface routing to the default gateway, `eth0` when the route
    /// cannot be read. `ip` is resolved on this process's `PATH`, as v2's
    /// `execSync("ip route show default")` shell did.
    fn primary_linux_interface(&mut self) -> &str {
        if self.primary_interface.is_none() {
            let found = run_bounded(
                "ip",
                &["route", "show", "default"],
                None,
                None,
                SAMPLER_TOOL_TIMEOUT,
            )
            .as_deref()
            .and_then(route_device);
            self.primary_interface = Some(found.unwrap_or_else(|| "eth0".to_string()));
        }
        self.primary_interface.as_deref().unwrap_or("eth0")
    }
}

/// Cumulative CPU jiffies, and how many of them the CPU was not working.
#[derive(Debug, Clone, Copy)]
struct CpuReading {
    idle: u64,
    total: u64,
}

/// The `cpu ` aggregate line of `/proc/stat`, or `None` when any field fails
/// to parse (v2: `fields.some((n) => !Number.isFinite(n))` → 0).
fn read_cpu_jiffies() -> Option<CpuReading> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let line = stat.lines().next()?;
    let fields: Option<Vec<u64>> = line
        .strip_prefix("cpu ")?
        .split_whitespace()
        .map(|field| field.parse::<u64>().ok())
        .collect();
    let fields = fields?;
    // user nice system idle iowait irq softirq steal …
    let idle = fields.get(3).copied().unwrap_or(0) + fields.get(4).copied().unwrap_or(0);
    Some(CpuReading {
        idle,
        total: fields.iter().sum(),
    })
}

/// One sample, for a caller that keeps no sampler.
#[must_use]
pub fn sample_host(platform: HostPlatform) -> HostSample {
    HostSampler::new(platform).sample()
}

/// The token after `dev` in an `ip route` line (v2 `/\bdev\s+(\S+)/`).
fn route_device(output: &str) -> Option<String> {
    output
        .split_whitespace()
        .skip_while(|field| *field != "dev")
        .nth(1)
        .map(str::to_string)
}

/// (used, total) bytes from `/proc/meminfo`.
#[must_use]
pub fn sample_linux_memory() -> (u64, u64) {
    let Some(meminfo) = std::fs::read_to_string("/proc/meminfo").ok() else {
        return (0, 0);
    };
    let total_kb = meminfo_kb(&meminfo, "MemTotal");
    let available_kb = meminfo_kb(&meminfo, "MemAvailable");
    // "Used" the way `free` reports it: everything the kernel cannot hand to a
    // new allocation without reclaiming first.
    (
        total_kb.saturating_sub(available_kb) * 1024,
        total_kb * 1024,
    )
}

/// The `kB` value of one `/proc/meminfo` key, read as v2's `^Key:\s+(\d+)`:
/// every row carries a ` kB` unit after the number, so the whole remainder
/// never parses.
fn meminfo_kb(meminfo: &str, key: &str) -> u64 {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

/// (used, total) bytes of the root filesystem, from `df -k /`, or `None` when
/// `df` could not be run. A row it printed but that does not parse is zeros,
/// as v2's `Number(..) * 1024` of a missing column is.
pub(super) fn read_disk() -> Option<(u64, u64)> {
    let out = run_bounded("df", &["-k", "/"], None, None, SAMPLER_TOOL_TIMEOUT)?;
    let row = out.lines().nth(1).unwrap_or_default().to_string();
    let fields: Vec<&str> = row.split_whitespace().collect();
    let blocks = |index: usize| {
        fields
            .get(index)
            .and_then(|field| field.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    Some((blocks(2), blocks(1)))
}

/// (used, total) bytes of the root filesystem, zeros when unreadable.
#[must_use]
pub fn sample_disk() -> (u64, u64) {
    read_disk().unwrap_or((0, 0))
}

/// Cumulative receive/transmit bytes for one interface, from `/proc/net/dev`.
///
/// Every interface row is searched (v2 loops every line): the first line with
/// a colon is usually `lo`, not the interface routing to the gateway.
#[must_use]
pub fn sample_linux_net(interface: &str) -> Option<NetCounters> {
    let dev = std::fs::read_to_string("/proc/net/dev").ok()?;
    let rest = dev
        .lines()
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.trim() == interface)
        .map(|(_, rest)| rest)?;
    // The counter can butt against the colon when it grows wide, so the split
    // is on the colon, then on whitespace. rx: bytes packets errs drop fifo
    // frame compressed multicast; tx: bytes follows.
    let fields: Vec<&str> = rest.split_whitespace().collect();
    Some(NetCounters {
        rx_bytes: fields.first()?.parse::<u64>().ok()?,
        tx_bytes: fields.get(8)?.parse::<u64>().ok()?,
    })
}

/// This process's own cgroup-v2 memory throttle state, or `None` when it is
/// not on cgroup v2, unlimited (`max`), or unreadable (v2 `sampleCgroupPressure`).
///
/// The host-wide numbers above look perfectly healthy while a unit is being
/// throttled by its own `MemoryHigh` (2026-08-01: ovh1 published "8.7 GB of
/// 33.6 GB used" from inside a cgroup sitting above `MemoryHigh=3G`), so the
/// throttle is reported beside the memory figure rather than inside it. The
/// cgroup directory is `roost_host::host_memory`'s, the one the terminal-core
/// ceiling reads.
#[must_use]
pub fn sample_cgroup_pressure() -> Option<CgroupPressure> {
    let base = cgroup2_base(&read_linux_memory_file("/proc/self/cgroup")?)?;
    let high = read_linux_memory_file(&format!("{base}/memory.high"))?;
    let high = high.trim();
    if high == "max" {
        return None;
    }
    let high_bytes = high.parse::<u64>().ok()?;
    let current_bytes = read_linux_memory_file(&format!("{base}/memory.current"))?
        .trim()
        .parse::<u64>()
        .ok()?;
    let events = read_linux_memory_file(&format!("{base}/memory.events"))?;
    let high_events = events
        .lines()
        .find_map(|line| line.strip_prefix("high "))
        .and_then(|rest| rest.parse::<u64>().ok())
        .unwrap_or(0);
    Some(CgroupPressure {
        current_bytes,
        high_bytes,
        high_events,
    })
}
