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

/// The share of host memory the worker's soft ceiling asks for, with a floor
/// and no cap. Every PTY session — and every build or agent it runs — lives
/// under this one ceiling, and crossing it throttles the keeper and the
/// coordinator link along with the session that crossed it, so a constant
/// sized for a laptop strangles the whole worker on a big host.
const WORKER_HIGH_PERCENT: u64 = 60;
const WORKER_HIGH_FLOOR: u64 = 3 * 1024 * 1024 * 1024;

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
            memory_high: format_memory(
                (host_total_bytes * WORKER_HIGH_PERCENT / 100).max(WORKER_HIGH_FLOOR),
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
const WORKER_TASKS_MAX: &str = "4096";

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
    fn the_worker_soft_ceiling_scales_with_a_large_host_instead_of_stopping_at_a_constant() {
        // 60% of 64 GiB. A capped value here is the ceiling that froze a
        // 62 GiB host's keeper while its sessions ran a release build.
        assert_eq!(ResourceLimits::worker(64 * GIB).memory_high, "39321M");
    }

    #[test]
    fn the_worker_soft_ceiling_keeps_its_floor_on_a_small_or_unknown_host() {
        assert_eq!(ResourceLimits::worker(4 * GIB).memory_high, "3G");
        assert_eq!(ResourceLimits::worker(0).memory_high, "3G");
    }

    #[test]
    fn a_whole_gibibyte_renders_as_g_and_a_fraction_as_m() {
        assert_eq!(format_memory(GIB), "1G");
        assert_eq!(format_memory(1536 * 1024 * 1024), "1536M");
    }
}
