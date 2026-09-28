//! The monotonic budget one terminal-control hop spends: how long the
//! coordinator still waits for a typed worker result, and the strictly smaller
//! slice of it the worker may spend before the keeper write.
//! Ports the hop-budget half of `apps/coord/src/workers/worker-send.ts`
//! (`startHopDeadline`, `workerBudgetMs`, and the constants; the 8 s stream-state
//! deadline goes with the dropped stream controller). Read by
//! `workers::terminal_send`, `workers::terminal_request` and the input/route lanes.

use tokio::time::Instant;

// The budgets nest strictly:
//   keeper reconciliation (6 s, worker)
//     < worker pre-write budget (coordinator remaining - WORKER_HOP_RESERVE_MS)
//       < coordinator result deadline (5 s input)
// so an inner expiry always reports back while its outer waiter still listens,
// and an outer expiry can never race an inner one into a fabricated verdict.

/// The coordinator's whole wait for one terminal-input (or prompt) result.
pub const INPUT_CONTROL_TIMEOUT_MS: u64 = 5_000;

/// Return-trip and decode headroom withheld from the worker's slice, so a
/// worker pre-write rejection still arrives before the coordinator stops waiting.
const WORKER_HOP_RESERVE_MS: i64 = 750;

/// Below this the remaining budget cannot survive the hop, so writing the frame
/// buys nothing but duplicate risk. Refusing here is provably clean: nothing was
/// sent, so nothing can have mutated.
const MIN_WORKER_BUDGET_MS: i64 = 250;

/// One hop's budget, measured from the instant it started.
///
/// RELATIVE, NEVER ABSOLUTE: no instant crosses the wire. The worker receives a
/// millisecond budget and measures it from its own monotonic origin, so the two
/// hosts' wall clocks may differ by any amount -- a step, an NTP slew
/// mid-request -- without changing which side expires. The clock is
/// `tokio::time::Instant`, which never reads the wall clock and which a test
/// pauses and advances instead of racing a real timer.
#[derive(Debug, Clone, Copy)]
pub struct HopDeadline {
    total_ms: u64,
    started_at: Instant,
}

impl HopDeadline {
    /// Start a deadline now.
    ///
    /// A command queued behind a slow lane spends real elapsed time from here,
    /// and a backwards wall-clock step cannot hand it a fresh full budget.
    #[must_use]
    pub fn start(total_ms: u64) -> Self {
        Self {
            total_ms,
            started_at: Instant::now(),
        }
    }

    /// The whole budget this deadline started with.
    #[must_use]
    pub fn total_ms(&self) -> u64 {
        self.total_ms
    }

    /// Milliseconds left before the outer waiter gives up; negative once past.
    ///
    /// Fractional, as v2's `performance.now()` arithmetic is: the worker slice
    /// floors it and the result wait ceils it, and rounding once here would
    /// make one of the two wrong by a millisecond.
    #[must_use]
    pub fn remaining_ms(&self) -> f64 {
        let elapsed_ms = self.started_at.elapsed().as_nanos() as f64 / 1_000_000.0;
        self.total_ms as f64 - elapsed_ms
    }
}

/// The worker's slice of what is left, or `None` when too little remains to
/// attempt the hop at all.
#[must_use]
pub fn worker_budget_ms(deadline: &HopDeadline) -> Option<u32> {
    let remaining = deadline.remaining_ms().floor() as i64;
    let budget = remaining.saturating_sub(WORKER_HOP_RESERVE_MS);
    if budget < MIN_WORKER_BUDGET_MS {
        return None;
    }
    u32::try_from(budget).ok()
}
