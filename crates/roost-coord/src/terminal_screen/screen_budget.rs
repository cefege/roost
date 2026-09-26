//! The residency ceilings and the per-socket send slice, derived from a
//! declared byte budget.
//!
//! Ported from `apps/coord/src/terminal/screen/terminal-screen-budget.ts`.
//! `main.ts` derives these once at boot and hands them to the screen hub and the
//! Sync listener; nothing here does I/O, so the arithmetic is directly testable.
//! Depends only on the wire-level maxima in `roost-protocol` plus
//! [`crate::terminal_screen::residency`]'s hard maxima.
//!
//! EVERY NUMBER HERE IS A LIMIT THAT DECIDES WHETHER A SCREEN DEGRADES OR
//! CORRUPTS, so none of them is rounded for tidiness. Each carries v2's reason.

use roost_protocol::cell::{CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_SPANS};
use roost_protocol::viewport::TERMINAL_MAX_ROWS;

use crate::terminal_screen::residency::{
    TERMINAL_SCREEN_MAX_RESIDENT_ROWS, TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
};

/// Retained heap of one charged span: measured at 220 B for a real unique-text
/// span, rounded to a 16 B size class.
pub const TERMINAL_SCREEN_BYTES_PER_SPAN: u64 = 224;
/// Retained heap of one charged row: message object 48 B + parent array slot
/// 8 B + the row's own spans array header 32 B.
pub const TERMINAL_SCREEN_BYTES_PER_ROW: u64 = 88;
/// Every charged replica is mirrored once, UNCHARGED, by the snapshot source
/// for as long as the cache lives, so a budget must reserve two copies of
/// everything it admits.
pub const TERMINAL_SCREEN_REPLICA_COPIES: u64 = 2;
/// Rows are ~2% of replica cost; this split keeps the row cap meaningful
/// without letting it consume span budget.
pub const TERMINAL_SCREEN_ROW_BUDGET_FRACTION: f64 = 0.05;
/// Share of the detected ceiling the replica pool may claim.
pub const TERMINAL_SCREEN_BUDGET_CEILING_FRACTION: f64 = 0.25;

/// A browser socket's real send buffer, sized so eight concurrent viewers
/// cannot exceed the replica budget. Floor: two whole snapshot parts.
pub const SYNC_BACKPRESSURE_BUDGET_SOCKETS: u64 = 8;
const SYNC_BACKPRESSURE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The two ceilings a budget resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalScreenCaps {
    pub max_resident_rows: u64,
    pub max_resident_spans: u64,
}

fn clamp(value: u64, low: u64, high: u64) -> u64 {
    value.min(high).max(low)
}

/// Budget bytes -> the two residency ceilings.
///
/// Clamped between "one worst-styled max-geometry session must always fit"
/// (`TERMINAL_MAX_ROWS` rows, `CELL_GRID_SNAPSHOT_MAX_SPANS` spans) and today's
/// hard maxima. A budget too small to hold one worst-case session is raised to
/// that, because refusing it would make a single 256-row terminal unpaintable.
#[must_use]
pub fn terminal_screen_caps(budget_bytes: u64) -> TerminalScreenCaps {
    let effective = budget_bytes / TERMINAL_SCREEN_REPLICA_COPIES;
    let row_bytes = (effective as f64 * TERMINAL_SCREEN_ROW_BUDGET_FRACTION).floor() as u64;
    let span_bytes = effective.saturating_sub(row_bytes);
    TerminalScreenCaps {
        max_resident_rows: clamp(
            row_bytes / TERMINAL_SCREEN_BYTES_PER_ROW,
            u64::from(TERMINAL_MAX_ROWS),
            TERMINAL_SCREEN_MAX_RESIDENT_ROWS,
        ),
        max_resident_spans: clamp(
            span_bytes / TERMINAL_SCREEN_BYTES_PER_SPAN,
            u64::from(CELL_GRID_SNAPSHOT_MAX_SPANS),
            TERMINAL_SCREEN_MAX_RESIDENT_SPANS,
        ),
    }
}

/// 25% of the cgroup/host ceiling unless the operator declared a budget.
#[must_use]
pub fn terminal_screen_budget_bytes(
    configured_budget_bytes: Option<u64>,
    ceiling_bytes: u64,
) -> u64 {
    match configured_budget_bytes {
        Some(declared) => declared,
        None => (ceiling_bytes as f64 * TERMINAL_SCREEN_BUDGET_CEILING_FRACTION).floor() as u64,
    }
}

/// One browser socket's share of the same budget, floored at two whole
/// snapshot parts and capped at 8 MiB.
#[must_use]
pub fn sync_backpressure_bytes(budget_bytes: u64) -> u64 {
    clamp(
        budget_bytes / SYNC_BACKPRESSURE_BUDGET_SOCKETS,
        u64::from(CELL_GRID_PART_MAX_BYTES) * 2,
        SYNC_BACKPRESSURE_MAX_BYTES,
    )
}
