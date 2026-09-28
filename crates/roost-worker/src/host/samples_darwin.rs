//! The macOS host sampler: `top`, `vm_stat`, `sysctl`, `df` and `netstat`.
//! Ports v2 `apps/worker/src/host/host-sample-darwin.ts`; called through
//! [`super::samples::HostSampler`] when the platform is macOS. Depends on
//! `super::tool_path` for the bounded runner.
//!
//! THE FAILURE GROUPING IS v2's. Memory and disk are read inside one v2 `try`:
//! a `vm_stat` or `hw.memsize` that cannot be run leaves memory AND disk at
//! zero, and a `df` that cannot be run leaves only the disk at zero. CPU and
//! the interface counters are read on their own and fail on their own.

use std::time::Duration;

use super::samples::{SAMPLER_TOOL_TIMEOUT, read_disk};
use super::tool_path::run_bounded;
use super::{HostSample, NetCounters};

/// 16384 is the Apple-silicon page size and a fourfold overstatement on an
/// Intel Mac. It is the fallback for an unreadable `hw.pagesize`, not the value.
const APPLE_SILICON_PAGE_SIZE: u64 = 16384;

/// v2 bounds `top -l 1` at two seconds; it prints one snapshot then exits.
const TOP_TIMEOUT: Duration = Duration::from_secs(2);

/// The two readings v2 memoizes on first use (`_pageSize`, `_primaryIface`).
#[derive(Debug, Default)]
pub(super) struct DarwinSampler {
    page_size: Option<u64>,
    primary_interface: Option<String>,
}

impl DarwinSampler {
    pub(super) const fn new() -> Self {
        Self {
            page_size: None,
            primary_interface: None,
        }
    }

    /// v2 `host-sample-darwin.ts` `sampleHost`.
    pub(super) fn sample(&mut self) -> HostSample {
        let cpu_pct = sample_cpu_pct();
        let mut sample = HostSample {
            cpu_pct,
            ..HostSample::default()
        };
        if let Some((mem_used, mem_total)) = self.sample_memory() {
            sample.mem_used_bytes = mem_used;
            sample.mem_total_bytes = mem_total;
            if let Some((disk_used, disk_total)) = read_disk() {
                sample.disk_used_bytes = disk_used;
                sample.disk_total_bytes = disk_total;
            }
        }
        let interface = self.primary_interface().to_string();
        sample.net = sample_darwin_net(&interface);
        sample
    }

    /// Apple's "Memory Used" is wired + compressed + active; Activity Monitor
    /// adds purgeable, which this approximates rather than pretends to match.
    /// `None` when `vm_stat` or `sysctl hw.memsize` could not be run.
    fn sample_memory(&mut self) -> Option<(u64, u64)> {
        let dump = run_bounded("vm_stat", &[], None, None, SAMPLER_TOOL_TIMEOUT)?;
        let pages = vm_stat_pages(&dump, "Pages active:")
            + vm_stat_pages(&dump, "Pages wired down:")
            + vm_stat_pages(&dump, "Pages occupied by compressor:");
        // Absolute path: a LaunchAgent's PATH has no /usr/sbin, and a bare
        // `sysctl` failed there every beat.
        let total = run_bounded(
            "/usr/sbin/sysctl",
            &["-n", "hw.memsize"],
            None,
            None,
            SAMPLER_TOOL_TIMEOUT,
        )?
        .trim()
        .parse::<u64>()
        .unwrap_or(0);
        Some((pages * self.page_size(), total))
    }

    /// The page size, read once: 16384 on Apple silicon, 4096 on Intel.
    fn page_size(&mut self) -> u64 {
        if let Some(size) = self.page_size {
            return size;
        }
        let size = run_bounded(
            "/usr/sbin/sysctl",
            &["-n", "hw.pagesize"],
            None,
            None,
            SAMPLER_TOOL_TIMEOUT,
        )
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|size| *size > 0)
        .unwrap_or(APPLE_SILICON_PAGE_SIZE);
        self.page_size = Some(size);
        size
    }

    /// The interface routing to the default gateway, `en0` when unreadable.
    fn primary_interface(&mut self) -> &str {
        if self.primary_interface.is_none() {
            let found = run_bounded(
                "/sbin/route",
                &["-n", "get", "default"],
                None,
                None,
                SAMPLER_TOOL_TIMEOUT,
            )
            .as_deref()
            .and_then(route_interface);
            self.primary_interface = Some(found.unwrap_or_else(|| "en0".to_string()));
        }
        self.primary_interface.as_deref().unwrap_or("en0")
    }
}

/// CPU % via `top -l 1 -n 0`, reported as `100 - idle` across all cores so a
/// single saturated core on an eight-core machine reads ~12.5%, matching
/// Activity Monitor. Zero when `top` cannot be run or its line does not parse.
fn sample_cpu_pct() -> f64 {
    run_bounded(
        "/usr/bin/top",
        &["-l", "1", "-n", "0"],
        None,
        None,
        TOP_TIMEOUT,
    )
    .as_deref()
    .and_then(top_idle_pct)
    .map_or(0.0, |idle| (100.0 - idle).clamp(0.0, 100.0))
}

/// The idle percentage of `top`'s `CPU usage: X% user, Y% sys, Z% idle` line
/// (v2 `/CPU usage:\s+[\d.]+%\s+user,\s+[\d.]+%\s+sys,\s+([\d.]+)%\s+idle/`).
#[must_use]
pub fn top_idle_pct(out: &str) -> Option<f64> {
    let (_, rest) = out.split_once("CPU usage:")?;
    let mut parts = rest.split(',');
    let labelled = |part: &str, label: &str| -> Option<f64> {
        let (number, after) = part.trim().split_once('%')?;
        if !after.trim_start().starts_with(label) || !is_decimal(number) {
            return None;
        }
        number.parse::<f64>().ok()
    };
    labelled(parts.next()?, "user")?;
    labelled(parts.next()?, "sys")?;
    labelled(parts.next()?, "idle").filter(|idle| idle.is_finite())
}

/// Digits and dots only, as `[\d.]+` admits.
fn is_decimal(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
}

/// The page count after `label` in a `vm_stat` dump, read as v2's
/// `/<label>\s+(\d+)/`: the value carries a trailing `.`, so only the leading
/// digits are the number.
fn vm_stat_pages(dump: &str, label: &str) -> u64 {
    let Some((_, rest)) = dump.split_once(label) else {
        return 0;
    };
    let digits: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse::<u64>().unwrap_or(0)
}

/// The token after `interface:` in `route -n get default` output.
fn route_interface(output: &str) -> Option<String> {
    let (_, rest) = output.split_once("interface:")?;
    rest.split_whitespace().next().map(str::to_string)
}

/// Cumulative receive/transmit bytes for one interface, from `netstat -ibn`.
///
/// The global form rather than `-I <iface>`: the per-interface form has been
/// observed to truncate to 32-bit counters on some macOS builds. A `<Link#N>`
/// row repeats the name with link-level counters and is skipped, and a row
/// whose byte columns do not parse is skipped too (v2 keeps looking).
#[must_use]
pub fn sample_darwin_net(interface: &str) -> Option<NetCounters> {
    let out = run_bounded(
        "/usr/sbin/netstat",
        &["-ibn"],
        None,
        None,
        SAMPLER_TOOL_TIMEOUT,
    )?;
    parse_netstat_bytes(&out, interface)
}

/// The first non-Link row for `interface` in `netstat -ibn` output whose
/// `Ibytes` (column 7) and `Obytes` (column 10) both parse.
#[must_use]
pub fn parse_netstat_bytes(out: &str, interface: &str) -> Option<NetCounters> {
    out.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.first() != Some(&interface)
            || fields
                .get(2)
                .is_some_and(|field| field.starts_with("<Link"))
        {
            return None;
        }
        Some(NetCounters {
            rx_bytes: fields.get(6)?.parse::<u64>().ok()?,
            tx_bytes: fields.get(9)?.parse::<u64>().ok()?,
        })
    })
}
