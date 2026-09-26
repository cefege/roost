//! The per-socket domain table: one slot per Sync domain, the limits each
//! domain and each half of the retention budget obeys, and the process-wide
//! monotonic source of domain generations.
//!
//! Owned by the Sync session. Split out of `session.rs` for the 400-line cap
//! and not for a boundary: every one of these is a property of a DOMAIN, and
//! the session that owns them is the only writer.

use std::sync::atomic::{AtomicU64, Ordering};

use roost_proto::SyncDomain;

use super::retained_frame::RetainedFrame;

/// How many Sync domains exist. Seven, one per non-`UNSPECIFIED` value in
/// `roost.v1.SyncDomain`; the table is fixed-size so a domain's state is an
/// index rather than a hash lookup on the send path.
pub const DOMAIN_SLOTS: usize = 7;

/// The slot `UNSPECIFIED` maps to, which is past the table and therefore never
/// indexed. A total mapping is what lets the terminal half of the session name
/// its own domain without an unwrap on the hot path.
pub const UNSPECIFIED_SLOT: usize = DOMAIN_SLOTS;

/// The domain's slot in the per-socket table.
#[must_use]
pub const fn domain_slot(domain: SyncDomain) -> usize {
    match domain {
        SyncDomain::Terminal => 0,
        SyncDomain::Workers => 1,
        SyncDomain::Workspaces => 2,
        SyncDomain::Tasks => 3,
        SyncDomain::Mcp => 4,
        SyncDomain::Pair => 5,
        SyncDomain::Audit => 6,
        SyncDomain::Unspecified => UNSPECIFIED_SLOT,
    }
}

/// Frames one domain may hold before admission resets it.
pub const DOMAIN_MAX_QUEUED_FRAMES: usize = 512;

/// Bytes one domain may hold before admission resets it.
pub const DOMAIN_MAX_QUEUED_BYTES: u64 = 4 * 1024 * 1024;

/// Frames the terminal half of one socket may retain.
pub const TERMINAL_MAX_RETAINED_FRAMES: usize = 512;

/// Bytes the terminal half of one socket may retain.
pub const TERMINAL_MAX_RETAINED_BYTES: u64 = 4 * 1024 * 1024;

/// Frames the non-terminal half of one socket may retain.
pub const NONTERMINAL_MAX_RETAINED_FRAMES: usize = 512;

/// Bytes the non-terminal half of one socket may retain.
pub const NONTERMINAL_MAX_RETAINED_BYTES: u64 = 4 * 1024 * 1024;

/// The sum of the two halves, which is also the socket's whole budget.
pub const AGGREGATE_MAX_RETAINED_FRAMES: usize =
    TERMINAL_MAX_RETAINED_FRAMES + NONTERMINAL_MAX_RETAINED_FRAMES;

/// The sum of the two halves in bytes.
pub const AGGREGATE_MAX_RETAINED_BYTES: u64 =
    TERMINAL_MAX_RETAINED_BYTES + NONTERMINAL_MAX_RETAINED_BYTES;

/// Frames one terminal lane may hold past its baseline before it rebaselines.
pub const TERMINAL_LANE_MAX_DELTA_FRAMES: usize = 32;

/// Bytes one terminal lane may hold past its baseline before it rebaselines.
pub const TERMINAL_LANE_MAX_DELTA_BYTES: u64 = 4 * 1024 * 1024;

/// The terminal frames a busy terminal cannot have, held back for state and
/// feed frames (`sync-ws-v2-state.ts:54-60`).
pub const TERMINAL_RELIABLE_RESERVE_FRAMES: usize = 64;

/// The same reserve in bytes.
pub const TERMINAL_RELIABLE_RESERVE_BYTES: u64 = 256 * 1024;

/// The cell sub-budget: the terminal budget minus the reliable reserve.
pub const TERMINAL_CELL_MAX_RETAINED_FRAMES: usize =
    TERMINAL_MAX_RETAINED_FRAMES - TERMINAL_RELIABLE_RESERVE_FRAMES;

/// The cell sub-budget in bytes.
pub const TERMINAL_CELL_MAX_RETAINED_BYTES: u64 =
    TERMINAL_MAX_RETAINED_BYTES - TERMINAL_RELIABLE_RESERVE_BYTES;

/// How long a non-cell frame may sit at a domain's head before it outranks
/// every cell, which is what stops a streaming terminal from starving the
/// domains that describe it (`sync-ws-v2-state.ts:61`).
pub const LOW_LANE_MAX_AGE_MS: u64 = 100;

/// How many frames one flush turn sends before yielding to the executor.
///
/// v2 flushed 64 frames per microtask and then armed a zero-delay timer, so a
/// browser tab that woke to a thousand queued frames still saw its own input
/// acknowledged between batches. In Rust every socket write is already a yield
/// point, so the same bound applies per turn and the "is there more" probe is
/// the same probe (`sync-ws-v2-egress.ts:275,354-360`).
pub const FLUSH_BATCH_FRAMES: usize = 64;

/// The process-wide monotonic source of domain generations.
///
/// v2 kept this in a module-level `let` seeded from `Date.now()`. Here it is a
/// value the process shell owns and hands to each socket, because a top-level
/// mutable global is state nobody can grep for an owner of. The seed still
/// comes from the epoch so a generation minted before a coordinator restart
/// cannot be mistaken for one minted after it, and `process_epoch` in the
/// subscribed frame is the belt to that braces.
#[derive(Debug)]
pub struct DomainGenerations {
    next: AtomicU64,
}

impl DomainGenerations {
    /// A generator whose first allocation is past every generation a
    /// coordinator started at or before `epoch_ms` could have minted.
    #[must_use]
    pub fn new(epoch_ms: u64) -> Self {
        Self {
            next: AtomicU64::new(epoch_ms.saturating_mul(1024)),
        }
    }

    /// The next generation. Monotonic across every socket in this process.
    pub fn allocate(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// One domain's generation, subscription, queue, and snapshot cutover.
#[derive(Debug)]
pub struct DomainState {
    /// Which domain this is. Stored rather than implied by the slot so the
    /// subscribed announcement and every log line name it.
    pub domain: SyncDomain,
    /// Bumped by every reset. A client ignores a frame from a generation other
    /// than the current one, which is what makes a reset safe to announce at
    /// any point in a frame's flight.
    pub generation: u64,
    /// Whether the client asked for this domain at all. Audit starts off.
    pub subscribed: bool,
    /// Whether the client has closed this domain's snapshot/live gap. Nothing
    /// is delivered for a domain that is not ready.
    pub ready: bool,
    /// Frames admitted for this domain, in the order they must go out.
    pub(in crate::sync_ws) queue: Vec<RetainedFrame>,
    /// The queue's charged bytes, kept beside it so admission is arithmetic
    /// rather than a sum over the queue.
    pub(in crate::sync_ws) queued_bytes: u64,
    /// Where the next retained seed is inserted: immediately before the live
    /// segment it must precede.
    pub(in crate::sync_ws) seed_insert_index: usize,
}
/// The domain a table slot holds, in the coordinator's own order.
pub(in crate::sync_ws) const fn domain_at_slot(slot: usize) -> SyncDomain {
    match slot {
        0 => SyncDomain::Terminal,
        1 => SyncDomain::Workers,
        2 => SyncDomain::Workspaces,
        3 => SyncDomain::Tasks,
        4 => SyncDomain::Mcp,
        5 => SyncDomain::Pair,
        _ => SyncDomain::Audit,
    }
}

/// Whether a table slot holds the audit domain, which is the one domain a
/// socket never subscribes to on its own.
pub(in crate::sync_ws) fn is_lazy_slot(slot: usize) -> bool {
    matches!(domain_at_slot(slot), SyncDomain::Audit)
}
