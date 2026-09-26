//! The cgroup ceilings a Linux unit carries, derived from the host's own
//! memory rather than fixed, because a fixed ceiling is no ceiling at all on a
//! box smaller than the constant. Called by service_spec.rs; the renderers read
//! the values and never re-derive them.
//!
//! The two roles do not get the same set. The coordinator owns no session
//! subtree, so a hard `MemoryMax` bounces the coordinator and nothing with it.
//! The worker owns every live PTY inside its cgroup, so a hard cap plus
//! `Restart=always` would let one fat session kill the unit and take every
//! other live session down with it — it gets `MemoryHigh`, which only
//! throttles, and `OOMPolicy=continue`, which closes the same hole against a
//! host-level kill of a single child.

/// The share of host memory the coordinator's soft ceiling asks for.
const COORD_HIGH_PERCENT: u64 = 45;
const COORD_HIGH_FLOOR: u64 = 384 * 1024 * 1024;
const COORD_HIGH_CAP: u64 = 1024 * 1024 * 1024;
const COORD_MAX_PERCENT: u64 = 70;
const COORD_MAX_FLOOR: u64 = 512 * 1024 * 1024;
const COORD_MAX_CAP: u64 = 2 * 1024 * 1024 * 1024;

/// The share of host memory the worker's soft ceiling asks for. The worker
/// starts further back than the coordinator because every session it holds
/// lives under that one ceiling.
const WORKER_HIGH_PERCENT: u64 = 55;
const WORKER_HIGH_FLOOR: u64 = 768 * 1024 * 1024;
const WORKER_HIGH_CAP: u64 = 3 * 1024 * 1024 * 1024;

/// The ceilings baked into a unit, already formatted the way systemd spells
/// them. `memory_max` is absent where a hard kill would take live work with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    /// The soft ceiling; crossing it throttles and reclaims.
    pub memory_high: String,
    /// The hard ceiling, or `None` where one is refused.
    pub memory_max: Option<String>,
    /// The task ceiling.
    pub tasks_max: String,
    /// The host total the ceilings were derived from, for the install's log
    /// line. Zero means detection failed and the caps were used directly.
    pub host_total_bytes: u64,
}

impl ResourceLimits {
    /// The coordinator's ceilings.
    pub fn coordinator(host_total_bytes: u64) -> Self {
        Self {
            memory_high: derive(
                host_total_bytes,
                COORD_HIGH_PERCENT,
                COORD_HIGH_FLOOR,
                COORD_HIGH_CAP,
            ),
            memory_max: Some(derive(
                host_total_bytes,
                COORD_MAX_PERCENT,
                COORD_MAX_FLOOR,
                COORD_MAX_CAP,
            )),
            tasks_max: COORDINATOR_TASKS_MAX.to_string(),
            host_total_bytes,
        }
    }

    /// The worker's ceilings, deliberately without a hard memory cap.
    pub fn worker(host_total_bytes: u64) -> Self {
        Self {
            memory_high: derive(
                host_total_bytes,
                WORKER_HIGH_PERCENT,
                WORKER_HIGH_FLOOR,
                WORKER_HIGH_CAP,
            ),
            memory_max: None,
            tasks_max: WORKER_TASKS_MAX.to_string(),
            host_total_bytes,
        }
    }
}

/// The coordinator's task ceiling: one task per connection plus headroom for
/// its own bookkeeping.
const COORDINATOR_TASKS_MAX: &str = "256";

/// The worker's task ceiling, which has to cover every PTY's shell and its
/// descendants, not one request.
const WORKER_TASKS_MAX: &str = "1024";

/// A percentage of the host total, clamped into `[floor, cap]` and rendered.
/// A total of zero means detection failed, which keeps the cap rather than
/// dividing by nothing.
fn derive(total_bytes: u64, percent: u64, floor_bytes: u64, cap_bytes: u64) -> String {
    let value = if total_bytes == 0 {
        cap_bytes
    } else {
        (total_bytes * percent / 100).clamp(floor_bytes, cap_bytes)
    };
    format_memory(value)
}

/// Whole gibibytes render as `<n>G`, so a large host's unit still reads the way
/// an operator wrote it; anything else is mebibytes, the finest granularity
/// worth emitting.
fn format_memory(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes.is_multiple_of(GIB) {
        format!("{}G", bytes / GIB)
    } else {
        format!("{}M", bytes / (1024 * 1024))
    }
}

/// The host's total memory in bytes, or zero when it cannot be read. Linux is
/// asked its own `/proc/meminfo` and macOS its own `sysctl`, because a host
/// that answers neither is a host this install will warn about rather than
/// refuse.
pub fn host_total_memory_bytes() -> u64 {
    read_linux_meminfo().unwrap_or_else(read_macos_memsize)
}

fn read_linux_meminfo() -> Option<u64> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let kib = meminfo
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|digits| digits.parse::<u64>().ok())?;
    kib.checked_mul(1024)
}

fn read_macos_memsize() -> u64 {
    let output = std::process::Command::new("sysctl")
        .args(["-n", "hw.memsize"])
        .output();
    match output {
        Ok(output) if output.status.success() => parse_decimal(&output.stdout),
        _ => 0,
    }
}

fn parse_decimal(stdout: &[u8]) -> u64 {
    std::str::from_utf8(stdout)
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{ResourceLimits, derive, format_memory};

    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn a_tiny_host_keeps_the_floor_so_the_service_can_still_serve() {
        // Half a gibibyte's share is well under the floor, so the floor is
        // what answers rather than a coordinator starved below the point where
        // it can serve at all.
        assert_eq!(derive(GIB / 2, 45, 384 << 20, GIB), "384M");
    }

    #[test]
    fn a_large_host_stops_at_the_cap() {
        assert_eq!(derive(64 * GIB, 45, 384 << 20, GIB), "1G");
        assert_eq!(derive(64 * GIB, 70, 512 << 20, 2 * GIB), "2G");
    }

    #[test]
    fn an_undetectable_total_keeps_the_cap_instead_of_dividing_by_nothing() {
        assert_eq!(derive(0, 45, 384 << 20, GIB), "1G");
    }

    #[test]
    fn the_worker_has_no_hard_cap_and_the_coordinator_does() {
        assert!(ResourceLimits::worker(8 * GIB).memory_max.is_none());
        assert_eq!(
            ResourceLimits::coordinator(8 * GIB).memory_max,
            Some("2G".to_string())
        );
    }

    #[test]
    fn a_whole_gibibyte_renders_as_g_and_a_fraction_as_m() {
        assert_eq!(format_memory(GIB), "1G");
        assert_eq!(format_memory(1536 * 1024 * 1024), "1536M");
    }
}
