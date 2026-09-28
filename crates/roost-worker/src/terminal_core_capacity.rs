//! Worker-local terminal-core admission and replacement headroom. Each core is
//! leased from construction through record teardown, an over-cap survivor set
//! is refused before any channel is touched, and heartbeat reads the snapshot.
//! Ports `apps/worker/src/terminal/terminal-core-capacity.ts`; how big the
//! admission is lives in [`sizing`]. Held by `session::lifecycle::SessionManager`;
//! called by `session::{spawn,respawn,resume}` and `runtime::adoption`.

pub mod sizing;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use roost_observability::{LogFields, SignalKind, signal};
use roost_protocol::wire::TerminalCoreCapacityReport;

pub use sizing::{
    ENV_WORKER_TERMINAL_CAP, TerminalCoreCapConfigError, WorkerTerminalCoreCapacityOptions,
    create_worker_terminal_core_capacity, default_terminal_core_capacity,
    terminal_core_cap_from_env,
};

pub const TERMINAL_CORE_CAPACITY_HARD_MAX: u32 = 500;
pub const TERMINAL_CORE_ALLOCATION_BYTES: u64 = 40 * 1024 * 1024;
pub const TERMINAL_CORE_CAPACITY_ERROR_CODE: &str = "terminal_core_capacity";
pub const TERMINAL_CORE_CAPACITY_ERROR_MESSAGE: &str = "terminal core capacity exhausted";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCoreAllocationKind {
    Fresh,
    Adoption,
    Replacement,
}

impl TerminalCoreAllocationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Adoption => "adoption",
            Self::Replacement => "replacement",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalCoreCapacityRefusalReason {
    Allocation(TerminalCoreAllocationKind),
    SurvivorCount,
}

impl TerminalCoreCapacityRefusalReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allocation(kind) => kind.as_str(),
            Self::SurvivorCount => "survivor_count",
        }
    }
}

/// The stable admission refusal, so a caller can stop safely without mistaking
/// an intentional capacity refusal for a keeper or terminal-core fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{}", TERMINAL_CORE_CAPACITY_ERROR_MESSAGE)]
pub struct TerminalCoreCapacityError {
    pub reason: TerminalCoreCapacityRefusalReason,
}

impl TerminalCoreCapacityError {
    pub fn code(&self) -> &'static str {
        TERMINAL_CORE_CAPACITY_ERROR_CODE
    }
}

/// A lease used outside the one order v2 admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TerminalCoreLeaseMisuse {
    #[error("terminal core lease cannot activate twice or after release")]
    NotPending,
    #[error("terminal core replacement completion is invalid")]
    InvalidReplacementCompletion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCoreCapacityOptions {
    pub effective_memory_ceiling_bytes: u64,
    pub boot_rss_bytes: u64,
    pub terminal_core_cap: Option<u32>,
}

#[derive(Debug, Default)]
struct LeaseBook {
    next_lease_id: u64,
    pending: HashSet<u64>,
    used: HashSet<u64>,
    replacement_lease: Option<u64>,
    refusal_count: u64,
}

/// One owner tracks every pending and resident core. A replacement lease keeps
/// the single serialized slot until its caller has torn down the old record.
#[derive(Debug)]
pub struct TerminalCoreCapacity {
    capacity: u32,
    effective_memory_ceiling_bytes: u64,
    boot_rss_bytes: u64,
    book: Mutex<LeaseBook>,
    /// The lease each live channel's core holds; removed exactly at teardown.
    residents: Mutex<HashMap<u16, TerminalCoreLease>>,
    self_handle: Weak<Self>,
}

/// One admitted core. Dropping it releases its slot, so every early return
/// between reservation and residency gives the slot back.
#[derive(Debug)]
pub struct TerminalCoreLease {
    lease_id: u64,
    allocation_kind: TerminalCoreAllocationKind,
    owner: Weak<TerminalCoreCapacity>,
}

impl TerminalCoreLease {
    pub fn allocation_kind(&self) -> TerminalCoreAllocationKind {
        self.allocation_kind
    }

    /// Pending → resident. The core now exists and is held by a record.
    pub fn activate(&self) -> Result<(), TerminalCoreLeaseMisuse> {
        match self.owner.upgrade() {
            Some(owner) => owner.activate(self),
            None => Err(TerminalCoreLeaseMisuse::NotPending),
        }
    }

    /// Give the slot back now rather than at scope end.
    pub fn release(self) {}
}

impl Drop for TerminalCoreLease {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.upgrade() {
            owner.release(self.lease_id, self.allocation_kind);
        }
    }
}

impl TerminalCoreCapacity {
    pub fn new(options: TerminalCoreCapacityOptions) -> Arc<Self> {
        let default_capacity = default_terminal_core_capacity(
            options.effective_memory_ceiling_bytes,
            options.boot_rss_bytes,
        );
        let capacity = options
            .terminal_core_cap
            .map_or(default_capacity, |cap| cap.min(default_capacity));
        tracing::info!(
            capacity,
            effective_memory_ceiling_bytes = options.effective_memory_ceiling_bytes,
            boot_rss_bytes = options.boot_rss_bytes,
            "terminal_core_capacity_initialized"
        );
        Arc::new_cyclic(|self_handle| Self {
            capacity,
            effective_memory_ceiling_bytes: options.effective_memory_ceiling_bytes,
            boot_rss_bytes: options.boot_rss_bytes,
            book: Mutex::new(LeaseBook::default()),
            residents: Mutex::new(HashMap::new()),
            self_handle: self_handle.clone(),
        })
    }

    /// The content-free heartbeat report (v2 `main.ts:312-324`).
    pub fn snapshot(&self) -> TerminalCoreCapacityReport {
        report(self, &lock(&self.book))
    }

    pub fn reserve(
        &self,
        kind: TerminalCoreAllocationKind,
    ) -> Result<TerminalCoreLease, TerminalCoreCapacityError> {
        let mut book = lock(&self.book);
        let leased = book.pending.len() + book.used.len();
        let capacity = self.capacity as usize;
        let refused = match kind {
            TerminalCoreAllocationKind::Replacement => {
                capacity == 0 || book.replacement_lease.is_some() || leased > capacity
            }
            TerminalCoreAllocationKind::Fresh | TerminalCoreAllocationKind::Adoption => {
                capacity == 0 || leased >= capacity
            }
        };
        if refused {
            return Err(self.refuse(
                &mut book,
                TerminalCoreCapacityRefusalReason::Allocation(kind),
            ));
        }
        book.next_lease_id += 1;
        let lease_id = book.next_lease_id;
        book.pending.insert(lease_id);
        if kind == TerminalCoreAllocationKind::Replacement {
            book.replacement_lease = Some(lease_id);
        }
        self.log_transition(&book, "terminal_core_capacity_reserved", kind);
        Ok(TerminalCoreLease {
            lease_id,
            allocation_kind: kind,
            owner: self.self_handle.clone(),
        })
    }

    /// Refuse a complete keeper survivor set before any channel is attached or
    /// a partial set of cores could be allocated.
    pub fn assert_can_adopt_survivors(
        &self,
        channel_count: usize,
    ) -> Result<(), TerminalCoreCapacityError> {
        let mut book = lock(&self.book);
        if book.pending.len() + book.used.len() + channel_count > self.capacity as usize {
            return Err(self.refuse(&mut book, TerminalCoreCapacityRefusalReason::SurvivorCount));
        }
        Ok(())
    }

    /// Free the replacement serialization once the OLD record is torn down; the
    /// new record's lease stays resident.
    pub fn complete_replacement(
        &self,
        lease: &TerminalCoreLease,
    ) -> Result<(), TerminalCoreLeaseMisuse> {
        let mut book = lock(&self.book);
        let owned = std::ptr::eq(lease.owner.as_ptr(), self);
        if !owned
            || book.replacement_lease != Some(lease.lease_id)
            || !book.used.contains(&lease.lease_id)
        {
            return Err(TerminalCoreLeaseMisuse::InvalidReplacementCompletion);
        }
        book.replacement_lease = None;
        self.log_transition(
            &book,
            "terminal_core_replacement_completed",
            lease.allocation_kind,
        );
        Ok(())
    }

    /// Activate `lease` as `channel_id`'s resident core.
    pub fn install_channel(
        &self,
        channel_id: u16,
        lease: TerminalCoreLease,
    ) -> Result<(), TerminalCoreLeaseMisuse> {
        self.activate(&lease)?;
        let displaced = lock(&self.residents).insert(channel_id, lease);
        if displaced.is_some() {
            tracing::warn!(
                channel_id,
                "a channel's resident core lease was displaced and released"
            );
        }
        Ok(())
    }

    /// v2 core re-proof: the new core becomes resident, the old one's slot is
    /// released, and the replacement slot is freed.
    pub fn replace_channel_lease(
        &self,
        channel_id: u16,
        lease: TerminalCoreLease,
    ) -> Result<(), TerminalCoreLeaseMisuse> {
        self.activate(&lease)?;
        let previous = lock(&self.residents).insert(channel_id, lease);
        drop(previous);
        self.complete_channel_replacement(channel_id)
    }

    /// Free the replacement slot `channel_id`'s resident lease holds.
    pub fn complete_channel_replacement(
        &self,
        channel_id: u16,
    ) -> Result<(), TerminalCoreLeaseMisuse> {
        let residents = lock(&self.residents);
        let lease = residents
            .get(&channel_id)
            .ok_or(TerminalCoreLeaseMisuse::InvalidReplacementCompletion)?;
        self.complete_replacement(lease)
    }

    /// Record teardown: the channel's core is gone, so is its slot.
    pub fn release_channel(&self, channel_id: u16) -> bool {
        let released = lock(&self.residents).remove(&channel_id);
        released.is_some()
    }

    fn activate(&self, lease: &TerminalCoreLease) -> Result<(), TerminalCoreLeaseMisuse> {
        let mut book = lock(&self.book);
        if !std::ptr::eq(lease.owner.as_ptr(), self) || !book.pending.remove(&lease.lease_id) {
            return Err(TerminalCoreLeaseMisuse::NotPending);
        }
        book.used.insert(lease.lease_id);
        self.log_transition(
            &book,
            "terminal_core_capacity_activated",
            lease.allocation_kind,
        );
        Ok(())
    }

    fn release(&self, lease_id: u64, kind: TerminalCoreAllocationKind) {
        let mut book = lock(&self.book);
        let released = book.pending.remove(&lease_id) || book.used.remove(&lease_id);
        if !released {
            return;
        }
        if book.replacement_lease == Some(lease_id) {
            book.replacement_lease = None;
        }
        self.log_transition(&book, "terminal_core_capacity_released", kind);
    }

    fn refuse(
        &self,
        book: &mut LeaseBook,
        reason: TerminalCoreCapacityRefusalReason,
    ) -> TerminalCoreCapacityError {
        book.refusal_count = book.refusal_count.saturating_add(1);
        let snapshot = report(self, book);
        tracing::warn!(
            reason = reason.as_str(),
            refusal_count = snapshot.refusal_count,
            capacity = snapshot.capacity,
            used = snapshot.used,
            pending = snapshot.pending,
            "terminal_core_capacity_refused"
        );
        signal::emit(
            SignalKind::TerminalCoreCapacity,
            LogFields::new()
                .set("reason", reason.as_str())
                .set("refusal_count", snapshot.refusal_count)
                .set("capacity", snapshot.capacity)
                .set("used", snapshot.used)
                .set("pending", snapshot.pending)
                .set("cooldownKey", "worker"),
        );
        TerminalCoreCapacityError { reason }
    }

    fn log_transition(
        &self,
        book: &LeaseBook,
        event: &'static str,
        kind: TerminalCoreAllocationKind,
    ) {
        let snapshot = report(self, book);
        tracing::debug!(
            allocation_kind = kind.as_str(),
            capacity = snapshot.capacity,
            used = snapshot.used,
            pending = snapshot.pending,
            overcommit_count = snapshot.overcommit_count,
            "{event}"
        );
    }
}

fn report(capacity: &TerminalCoreCapacity, book: &LeaseBook) -> TerminalCoreCapacityReport {
    let used = book.used.len() as u64;
    let pending = book.pending.len() as u64;
    let leased = used + pending;
    TerminalCoreCapacityReport {
        used: saturating_u32(used),
        pending: saturating_u32(pending),
        capacity: capacity.capacity,
        estimated_reserved_bytes: leased.saturating_mul(TERMINAL_CORE_ALLOCATION_BYTES),
        effective_memory_ceiling_bytes: capacity.effective_memory_ceiling_bytes,
        boot_rss_bytes: capacity.boot_rss_bytes,
        overcommit_count: saturating_u32(leased.saturating_sub(u64::from(capacity.capacity))),
        refusal_count: book.refusal_count,
    }
}

fn saturating_u32(value: u64) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
