//! The memory ceiling a long-lived service sizes itself against: the tightest
//! finite cgroup2 `memory.high`/`memory.max` this process runs under, else
//! total host memory, plus this process's own resident set. Ports the
//! TypeScript `packages/host/src/host-memory.ts` and the Node `totalmem()` /
//! `memoryUsage().rss` reads of `apps/worker/src/terminal/terminal-core-capacity.ts`.
//! Called by the worker's `terminal_core_capacity`; depends on std and roost-platform.

use std::process::Command;

use roost_platform::HostPlatform;

/// Reads one Linux memory-control file; `None` stands for the TypeScript
/// reader's throw.
pub type MemoryFileReader<'a> = &'a dyn Fn(&str) -> Option<String>;

/// `Number.MAX_SAFE_INTEGER`: the TypeScript original refuses a cgroup limit it
/// cannot hold exactly.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// This process's cgroup2 directory, from `/proc/self/cgroup`'s `0::` line;
/// `None` when the process is not on the unified (cgroup2) hierarchy.
pub fn cgroup2_base(own_cgroup: &str) -> Option<String> {
    match own_cgroup.lines().find_map(|line| line.strip_prefix("0::")) {
        Some("/") => Some("/sys/fs/cgroup".to_owned()),
        Some(path) if path.starts_with('/') => Some(format!("/sys/fs/cgroup{path}")),
        _ => None,
    }
}

/// The tightest finite cgroup2 limit, else `host_memory_bytes`. Any unreadable
/// control file answers host memory, exactly as the TypeScript `catch` does.
pub fn effective_linux_memory_ceiling_bytes(
    host_memory_bytes: u64,
    read_text: MemoryFileReader<'_>,
) -> u64 {
    let Some(base) = read_text("/proc/self/cgroup").map(|own| cgroup2_base(&own)) else {
        return host_memory_bytes;
    };
    let Some(base) = base else {
        return host_memory_bytes;
    };
    let (Some(high), Some(max)) = (
        read_text(&format!("{base}/memory.high")),
        read_text(&format!("{base}/memory.max")),
    ) else {
        return host_memory_bytes;
    };
    [
        finite_cgroup_memory_limit(&high),
        finite_cgroup_memory_limit(&max),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(host_memory_bytes)
}

/// Only Linux exposes a cgroup limit; every other platform is bounded by host
/// memory alone.
pub fn effective_memory_ceiling_bytes(platform: HostPlatform, host_memory_bytes: u64) -> u64 {
    match platform {
        HostPlatform::Linux => {
            effective_linux_memory_ceiling_bytes(host_memory_bytes, &read_linux_memory_file)
        }
        HostPlatform::MacOs | HostPlatform::Windows => host_memory_bytes,
    }
}

/// The host's total memory (Node's `os.totalmem()`), zero when it cannot be read.
pub fn host_total_memory_bytes(platform: HostPlatform) -> u64 {
    match platform {
        HostPlatform::Linux => read_linux_memory_file("/proc/meminfo")
            .and_then(|meminfo| status_kib(&meminfo, "MemTotal:"))
            .unwrap_or(0),
        HostPlatform::MacOs => command_decimal("sysctl", &["-n", "hw.memsize"]).unwrap_or(0),
        HostPlatform::Windows => 0,
    }
}

/// This process's resident set (Node's `process.memoryUsage().rss`), zero when it
/// cannot be read.
pub fn process_rss_bytes(platform: HostPlatform) -> u64 {
    match platform {
        HostPlatform::Linux => read_linux_memory_file("/proc/self/status")
            .and_then(|status| status_kib(&status, "VmRSS:"))
            .unwrap_or(0),
        HostPlatform::MacOs => {
            let pid = std::process::id().to_string();
            command_decimal("ps", &["-o", "rss=", "-p", &pid])
                .and_then(|kib| kib.checked_mul(1024))
                .unwrap_or(0)
        }
        HostPlatform::Windows => 0,
    }
}

/// The production reader: the file's text, or `None` when it cannot be read.
pub fn read_linux_memory_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn finite_cgroup_memory_limit(value: &str) -> Option<u64> {
    let normalized = value.trim();
    if normalized.is_empty() || !normalized.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    normalized
        .parse::<u64>()
        .ok()
        .filter(|bytes| *bytes <= MAX_SAFE_INTEGER)
}

/// A `/proc` `Key:   <n> kB` row, in bytes.
fn status_kib(text: &str, key: &str) -> Option<u64> {
    text.lines()
        .find_map(|line| line.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|digits| digits.parse::<u64>().ok())
        .and_then(|kib| kib.checked_mul(1024))
}

fn command_decimal(program: &str, args: &[&str]) -> Option<u64> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    std::str::from_utf8(&output.stdout)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}
