//! The raw counters a heartbeat ships: CPU, memory, disk and interface bytes,
//! read from this host. The rate, the sixty-second cache and the wire shape
//! belong to the heartbeat; this module only reads, so two samplers on two
//! platforms cannot disagree about what a sample means. Depends on
//! `roost_host::HostPlatform` and on `std` — and on nothing here.
//!
//! NEVER FAILS. A sampler that returned an error would mean a heartbeat is not
//! sent, so a machine that cannot answer `/proc` or `vm_stat` would vanish from
//! the fleet view rather than report zeros. Every failure here is a zero.

use std::path::PathBuf;

use roost_host::HostPlatform;

use super::tool_path::run;
use super::{HostSample, NetCounters};

/// 16384 is the Apple-silicon page size and a fourfold overstatement on an
/// Intel Mac. It is the fallback for an unreadable `hw.pagesize`, not the value.
const APPLE_SILICON_PAGE_SIZE: u64 = 16384;

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
/// Stateful because two of the readings cannot be taken in one pass: CPU
/// percentage is a difference between two readings, and the interface to watch
/// comes from a route lookup whose answer does not change. Holding that on the
/// sampler is what keeps it off a module-level global, where a second sampler
/// and a test would fight over the same slot.
pub struct HostSampler {
    platform: HostPlatform,
    previous_cpu: Option<CpuReading>,
    primary_interface: Option<String>,
    page_size: Option<u64>,
}

impl HostSampler {
    #[must_use]
    pub const fn new(platform: HostPlatform) -> Self {
        Self {
            platform,
            previous_cpu: None,
            primary_interface: None,
            page_size: None,
        }
    }

    /// One sample of this host's raw counters.
    ///
    /// The first CPU reading after a worker starts is zero, and that is not a
    /// defect: `/proc/stat` holds jiffies since boot, so a single read says
    /// nothing about current load.
    pub fn sample(&mut self) -> HostSample {
        let cpu_pct = self.sample_cpu_pct();
        let (mem_used_bytes, mem_total_bytes) = self.sample_memory();
        let (disk_used_bytes, disk_total_bytes) = sample_disk();
        let interface = self.primary_interface().to_string();
        HostSample {
            cpu_pct,
            mem_used_bytes,
            mem_total_bytes,
            disk_used_bytes,
            disk_total_bytes,
            net: match self.platform {
                HostPlatform::MacOs => sample_darwin_net(&interface),
                HostPlatform::Linux => sample_linux_net(&interface),
                HostPlatform::Windows => None,
            },
        }
    }

    fn sample_cpu_pct(&mut self) -> f64 {
        let Some(reading) = self.read_cpu_jiffies() else {
            return 0.0;
        };
        let Some(previous) = self.previous_cpu.replace(reading) else {
            return 0.0;
        };
        let total_delta = reading.total.saturating_sub(previous.total);
        let idle_delta = reading.idle.saturating_sub(previous.idle);
        if total_delta == 0 {
            return 0.0;
        }
        let busy_pct = 100.0 * (1.0 - idle_delta as f64 / total_delta as f64);
        busy_pct.clamp(0.0, 100.0).round()
    }

    /// The `cpu ` aggregate line of `/proc/stat`, or `None`.
    fn read_cpu_jiffies(&self) -> Option<CpuReading> {
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

    /// (used, total) bytes of physical memory, the way `free` reports them.
    fn sample_memory(&self) -> (u64, u64) {
        match self.platform {
            HostPlatform::MacOs => self.sample_darwin_memory(),
            HostPlatform::Linux => sample_linux_memory(),
            HostPlatform::Windows => (0, 0),
        }
    }

    /// Apple's "Memory Used" is wired + compressed + active; Activity Monitor
    /// adds purgeable, which this approximates rather than pretends to match.
    fn sample_darwin_memory(&self) -> (u64, u64) {
        let Some(dump) = run("vm_stat", &[], None) else {
            return (0, 0);
        };
        let pages = vm_stat_value(&dump, "Pages active")
            + vm_stat_value(&dump, "Pages wired down")
            + vm_stat_value(&dump, "Pages occupied by compressor");
        let total = run("/usr/sbin/sysctl", &["-n", "hw.memsize"], None)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0);
        (pages * self.page_size(), total)
    }

    /// The page size, read once: 16384 on Apple silicon, 4096 on Intel.
    fn page_size(&mut self) -> u64 {
        if let Some(size) = self.page_size {
            return size;
        }
        let size = run("/usr/sbin/sysctl", &["-n", "hw.pagesize"], None)
            .and_then(|value| value.trim().parse::<u64>().ok())
            .filter(|size| *size > 0)
            .unwrap_or(APPLE_SILICON_PAGE_SIZE);
        self.page_size = Some(size);
        size
    }

    /// The interface routing to the default gateway, with the platform's
    /// conventional name as the answer when the route cannot be read.
    fn primary_interface(&mut self) -> &str {
        if self.primary_interface.is_none() {
            let found = match self.platform {
                HostPlatform::MacOs => run("/sbin/route", &["-n", "get", "default"], None)
                    .as_deref()
                    .and_then(|out| after_marker(out, "interface:")),
                HostPlatform::Linux => run("/usr/sbin/ip", &["route", "show", "default"], None)
                    .as_deref()
                    .and_then(route_device),
                HostPlatform::Windows => None,
            };
            self.primary_interface =
                Some(found.unwrap_or_else(|| fallback_interface(self.platform).to_string()));
        }
        self.primary_interface
            .as_deref()
            .unwrap_or_else(|| fallback_interface(self.platform))
    }
}

/// The interface name a platform's sampler assumes when the route is unreadable.
fn fallback_interface(platform: HostPlatform) -> &'static str {
    match platform {
        HostPlatform::MacOs => "en0",
        _ => "eth0",
    }
}

/// Cumulative CPU jiffies, and how many of them the CPU was not working.
#[derive(Debug, Clone, Copy)]
struct CpuReading {
    idle: u64,
    total: u64,
}

/// One sample, for a caller that keeps no sampler.
#[must_use]
pub fn sample_host(platform: HostPlatform) -> HostSample {
    HostSampler::new(platform).sample()
}

/// The `Pages <label>: <count>` value of a `vm_stat` dump.
fn vm_stat_value(dump: &str, label: &str) -> u64 {
    dump.lines()
        .find_map(|line| line.strip_prefix(label))
        .and_then(|rest| rest.trim_start_matches(':').trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// The trimmed text after `marker` on the first line of `text` carrying it.
fn after_marker(text: &str, marker: &str) -> Option<String> {
    text.lines()
        .find(|line| line.contains(marker))
        .and_then(|line| line.split_once(marker))
        .map(|(_, rest)| rest.trim().to_string())
}

/// The `dev` token of an `ip route` line, which is the interface after `dev`.
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

fn meminfo_kb(meminfo: &str, key: &str) -> u64 {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.trim_start_matches(':').trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// (used, total) bytes of the root filesystem, from `df -k /`.
#[must_use]
pub fn sample_disk() -> (u64, u64) {
    let Some(row) =
        run("df", &["-k", "/"], None).and_then(|out| out.lines().nth(1).map(str::to_string))
    else {
        return (0, 0);
    };
    let fields: Vec<&str> = row.split_whitespace().collect();
    let blocks = |index: usize| {
        fields
            .get(index)
            .and_then(|field| field.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (blocks(2), blocks(1))
}

/// Cumulative receive/transmit bytes for one interface, from `/proc/net/dev`.
#[must_use]
pub fn sample_linux_net(interface: &str) -> Option<NetCounters> {
    let dev = std::fs::read_to_string("/proc/net/dev").ok()?;
    let rest = dev
        .lines()
        .find_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.trim() == interface)
        .map(|(_, rest)| rest)?;
    let fields: Option<Vec<u64>> = rest
        .split_whitespace()
        .map(|field| field.parse::<u64>().ok())
        .collect();
    let fields = fields?;
    // rx: bytes packets errs drop fifo frame compressed multicast; tx follows
    // its own eight.
    Some(NetCounters {
        rx_bytes: *fields.first()?,
        tx_bytes: *fields.get(8)?,
    })
}

/// Cumulative receive/transmit bytes for one interface, from `netstat -ibn`.
///
/// The global form rather than `-I <iface>`: the per-interface form has been
/// observed to truncate to 32-bit counters on some macOS builds, and this is
/// the base a per-second rate is taken from.
#[must_use]
pub fn sample_darwin_net(interface: &str) -> Option<NetCounters> {
    let out = run("/usr/sbin/netstat", &["-ibn"], None)?;
    for line in out.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() != Some(&interface) {
            continue;
        }
        // A `<Link#N>` row repeats the interface name with link-level counters;
        // the first non-Link row is the interface's own.
        if fields
            .get(2)
            .is_some_and(|field| field.starts_with("<Link"))
        {
            continue;
        }
        return Some(NetCounters {
            rx_bytes: fields.get(6)?.parse::<u64>().ok()?,
            tx_bytes: fields.get(9)?.parse::<u64>().ok()?,
        });
    }
    None
}

/// This process's own cgroup-v2 memory throttle state, or `None`.
///
/// The host-wide numbers above look perfectly healthy while a unit is being
/// throttled by its own `MemoryHigh` (2026-08-01: ovh1 published "8.7 GB of
/// 33.6 GB used" from inside a cgroup sitting above `MemoryHigh=3G`), so the
/// throttle is reported beside the memory figure rather than inside it.
#[must_use]
pub fn sample_cgroup_pressure() -> Option<CgroupPressure> {
    let base = cgroup_v2_base()?;
    let high = read_trimmed(&base.join("memory.high"))?;
    if high == "max" {
        return None;
    }
    let high_bytes = high.parse::<u64>().ok()?;
    let current_bytes = read_trimmed(&base.join("memory.current"))?
        .parse::<u64>()
        .ok()?;
    let events = std::fs::read_to_string(base.join("memory.events")).ok()?;
    let high_events = events
        .lines()
        .find_map(|line| line.strip_prefix("high "))
        .and_then(|rest| rest.trim().parse::<u64>().ok())
        .unwrap_or(0);
    Some(CgroupPressure {
        current_bytes,
        high_bytes,
        high_events,
    })
}

/// This process's cgroup-v2 directory, or `None` when it is not on v2.
fn cgroup_v2_base() -> Option<PathBuf> {
    let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let path = own
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
        .filter(|path| !path.is_empty())?;
    Some(PathBuf::from("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

fn read_trimmed(path: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
}
