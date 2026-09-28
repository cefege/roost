//! How big the worker's terminal-core admission is: the operator cap from
//! `ROOST_WORKER_TERMINAL_CAP`, the host-derived steady-state cap, and the one
//! boot-time construction from this host's memory ceiling and resident set.
//! Ports `defaultTerminalCoreCapacity`/`createWorkerTerminalCoreCapacity` of
//! `apps/worker/src/terminal/terminal-core-capacity.ts` and `parseTerminalCoreCap`
//! of `apps/worker/src/host/config.ts`. Called by `runtime::session_stack`.

use std::sync::Arc;

use roost_host::{EnvSource, HostPlatform, host_memory};

use super::{
    TERMINAL_CORE_ALLOCATION_BYTES, TERMINAL_CORE_CAPACITY_HARD_MAX, TerminalCoreCapacity,
    TerminalCoreCapacityOptions,
};

/// The operator's upper bound on the host-derived cap (v2 `config.ts:79`).
pub const ENV_WORKER_TERMINAL_CAP: &str = "ROOST_WORKER_TERMINAL_CAP";

/// `ROOST_WORKER_TERMINAL_CAP` was set to something other than a decimal u32.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("ROOST_WORKER_TERMINAL_CAP must be a nonnegative decimal integer, not {value:?}")]
pub struct TerminalCoreCapConfigError {
    pub value: String,
}

/// The operator cap, or `None` when unset. No sign, no leading zero, at most
/// `0xffff_ffff`, exactly v2's `parseTerminalCoreCap`.
pub fn terminal_core_cap_from_env(
    env: &dyn EnvSource,
) -> Result<Option<u32>, TerminalCoreCapConfigError> {
    let Some(value) = env.get(ENV_WORKER_TERMINAL_CAP) else {
        return Ok(None);
    };
    let digits = !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit());
    let canonical = value == "0" || (digits && !value.starts_with('0'));
    match value.parse::<u32>() {
        Ok(cap) if canonical => Ok(Some(cap)),
        _ => Err(TerminalCoreCapConfigError { value }),
    }
}

/// The host-derived steady-state cap: 70% of the ceiling less boot RSS and one
/// allocation of headroom, in whole allocations, never above the hard max.
pub fn default_terminal_core_capacity(
    effective_memory_ceiling_bytes: u64,
    boot_rss_bytes: u64,
) -> u32 {
    let ceiling = effective_memory_ceiling_bytes;
    let seventy_percent = (ceiling / 10) * 7 + (ceiling % 10) * 7 / 10;
    let allocatable = seventy_percent
        .saturating_sub(boot_rss_bytes)
        .saturating_sub(TERMINAL_CORE_ALLOCATION_BYTES);
    let cores = (allocatable / TERMINAL_CORE_ALLOCATION_BYTES)
        .min(u64::from(TERMINAL_CORE_CAPACITY_HARD_MAX));
    u32::try_from(cores).unwrap_or(TERMINAL_CORE_CAPACITY_HARD_MAX)
}

/// What the composition root knows at boot; `None` reads the current host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkerTerminalCoreCapacityOptions {
    pub platform: HostPlatform,
    pub terminal_core_cap: Option<u32>,
    pub host_memory_bytes: Option<u64>,
    pub boot_rss_bytes: Option<u64>,
}

/// Construct the worker-owned capacity state from the current host, once, at
/// boot (v2 `createWorkerTerminalCoreCapacity`).
pub fn create_worker_terminal_core_capacity(
    options: WorkerTerminalCoreCapacityOptions,
) -> Arc<TerminalCoreCapacity> {
    let host_memory_bytes = options
        .host_memory_bytes
        .unwrap_or_else(|| host_memory::host_total_memory_bytes(options.platform));
    TerminalCoreCapacity::new(TerminalCoreCapacityOptions {
        effective_memory_ceiling_bytes: host_memory::effective_memory_ceiling_bytes(
            options.platform,
            host_memory_bytes,
        ),
        boot_rss_bytes: options
            .boot_rss_bytes
            .unwrap_or_else(|| host_memory::process_rss_bytes(options.platform)),
        terminal_core_cap: options.terminal_core_cap,
    })
}
