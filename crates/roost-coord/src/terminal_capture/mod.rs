//! Opt-in terminal incident capture on the coordinator: the authenticated
//! bridge behind DiagSnapshot's `terminal_capture`, the lease registry, the
//! bounded coordinator cell records, and the correlated worker call.
//! Ports `apps/coord/src/terminal/capture/` (`terminal-capture.ts`,
//! `terminal-capture-lease.ts`, `terminal-capture-recorder.ts`,
//! `terminal-capture-worker-call.ts`). Shapes, limits, validators, the command
//! and result, and the coordinator section all come from
//! `roost_protocol::terminal_capture`; this domain owns only the behaviour.
//!
//! One `Arc<TerminalCaptureRuntime>` per coordinator: the leases and the
//! recorder they arm are one table, so no lease transition can leave records
//! behind. Entered by `diagnostics::diag_snapshot` (the bridge) and by the
//! screen hub's accepted-frame hook (`TerminalCaptureRuntime::recorder`).

pub mod bridge;
pub mod freeze;
pub mod lease;
pub mod recorder;
mod session_scope;
pub mod worker_call;

use std::sync::Mutex;

use crate::terminal_capture::lease::LeaseTables;
use crate::terminal_capture::recorder::CoordinatorRecorder;

/// The capture state one coordinator process holds.
#[derive(Debug, Default)]
pub struct TerminalCaptureRuntime {
    leases: Mutex<LeaseTables>,
    /// The per-session cell records the leases arm.
    pub recorder: CoordinatorRecorder,
}

impl TerminalCaptureRuntime {
    /// A coordinator holding no lease and recording nothing.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Server time, in wall-clock milliseconds: leases expire on it.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }
}
