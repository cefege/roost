//! The two per-channel ordering lanes, and the table both are keyed in.
//! `session::lifecycle` serializes a stream transaction and a kill through
//! [`ControlLanes::enqueue`]; a resize, an input and a query reply take the
//! write-ordering lane through [`ControlLanes::admit`], whose tickets
//! `super::keeper_admission` owns. Depends on `roost_protocol` for the channel
//! id — and on nothing that depends on it back.
//!
//! TWO LANES, BECAUSE THEY ANSWER DIFFERENT QUESTIONS, AND COLLAPSING THEM IS
//! THE BUG. The CONTROL lane is MUTUAL EXCLUSION: a live resize owns the
//! synchronous result-frame boundary until the existing core is aligned, so a
//! kill and a resize may not interleave. The ADMISSION lane is RECEIVE-ORDER:
//! it makes keeper writes land in the order they were made. A stream
//! transaction releases admission as soon as its resize request is WRITTEN, so
//! input waits for the ordered boundary and never for the resize's
//! acknowledgement or its snapshot transfer. One lane serialising both would
//! make every keystroke wait on a resize acknowledgement.
//!
//! A LANE NEVER POISONS. A transaction that fails, panics or is dropped
//! releases its permit on the way out, because the alternative is a channel
//! whose only remaining transitions are the ones that do not touch the grid.
//! A permit rather than a held lock is what makes that automatic: there is no
//! guard for a panic to strand.
//!
//! THE ADMISSION GATE FAILS CLOSED. A keeper an update is about to replace must
//! not absorb one more byte, so [`ControlLanes::admit`] refuses a terminal write
//! outright once a replacement is prepared rather than queueing it behind a
//! connection that is about to be gone.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use roost_protocol::wire::brand::ChannelId;

use super::keeper_admission::{
    Admission, AdmissionKind, AdmissionTicket, KEEPER_UPDATE_WRITE_REFUSAL,
};

/// A whole transaction that owns the grid and may not overlap another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ControlKind {
    /// Applying a coordinator stream request to a live core.
    TerminalStream,
    /// Ending the channel.
    TerminalKill,
}

impl ControlKind {
    /// The name a log line and a diagnostic report both spell it as.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TerminalStream => "terminal_stream",
            Self::TerminalKill => "terminal_kill",
        }
    }

    const fn report_index(self) -> u32 {
        match self {
            Self::TerminalStream => 1,
            Self::TerminalKill => 2,
        }
    }

    /// The name a report's numeric field means, or nothing when the lane is idle.
    pub fn from_report_index(index: u32) -> Option<Self> {
        match index {
            1 => Some(Self::TerminalStream),
            2 => Some(Self::TerminalKill),
            _ => None,
        }
    }
}

/// Both lanes, keyed by the keeper's channel id.
///
/// Cheap to share through an `Arc`: the composition root owns one and hands it
/// to everything that writes to a channel.
#[derive(Debug, Default)]
pub struct ControlLanes {
    control: Mutex<HashMap<ChannelId, Arc<Lane>>>,
    admission: Mutex<HashMap<ChannelId, Arc<Lane>>>,
    keeper_update_prepared: AtomicBool,
}

impl ControlLanes {
    /// Two empty lane tables.
    pub fn new() -> Self {
        Self::default()
    }

    /// Serialize one whole terminal-control transaction, and return whatever the
    /// caller produced.
    ///
    /// A transaction that panics releases its permit on the way out, so the next
    /// one is not queued behind a lane nobody will ever drain.
    pub async fn enqueue<F, Fut, T>(&self, channel_id: ChannelId, kind: ControlKind, run: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let lane = self.lane(channel_id, &self.control);
        let queued = lane.waiting.fetch_add(1, Ordering::AcqRel) + 1;
        let Some(permit) = lane.gate.clone().acquire_owned().await.ok() else {
            // Unreachable while nothing closes the semaphore, and a lane that
            // cannot be entered must not be entered silently.
            tracing::error!(channel_id = ?channel_id, kind = kind.as_str(), "a control lane gate was closed");
            lane.waiting.fetch_sub(1, Ordering::AcqRel);
            self.forget_if_idle(channel_id, &lane, &self.control);
            return run().await;
        };
        lane.running.store(kind.report_index(), Ordering::Relaxed);
        lane.running_since.store(mono_ms(), Ordering::Relaxed);
        tracing::debug!(
            channel_id = ?channel_id,
            kind = kind.as_str(),
            queued,
            "a terminal control transaction took the lane"
        );
        let outcome = run().await;
        lane.running.store(0, Ordering::Relaxed);
        lane.running_since.store(0, Ordering::Relaxed);
        drop(permit);
        // The count goes down AFTER the permit is dropped, so a lane is only
        // ever considered idle once the transaction ahead of it has genuinely
        // left — which is what keeps a live lane from being removed from under
        // a waiter.
        lane.waiting.fetch_sub(1, Ordering::AcqRel);
        self.forget_if_idle(channel_id, &lane, &self.control);
        outcome
    }

    /// Wait for the transaction currently holding the control lane to finish.
    ///
    /// Public because the diagnostic paths and the browser-command readers must
    /// both observe a SETTLED core: a dims-change claim rebuilds a fresh core
    /// inside its transaction, and serving a page mid-rebuild hands out rows the
    /// imminent reframe invalidates.
    pub async fn settled(&self, channel_id: ChannelId) {
        let Some(lane) = self.lock(&self.control).get(&channel_id).cloned() else {
            return;
        };
        drop(lane.gate.acquire().await);
    }

    /// Take the write-ordering lane, or be refused outright.
    ///
    /// A refusal is FINAL for this attempt: the caller must surface it as a
    /// pre-write rejection rather than write to the keeper anyway. A ticket
    /// releases at the ORDERING BOUNDARY, which is the moment the write is on
    /// the wire — not its acknowledgement, and not the snapshot that follows.
    pub fn admit(&self, channel_id: ChannelId, kind: AdmissionKind) -> Admission {
        if kind.blocked_by_keeper_update() && self.keeper_update_prepared.load(Ordering::Acquire) {
            tracing::warn!(
                channel_id = ?channel_id,
                kind = kind.as_str(),
                "a terminal write was refused because a keeper replacement is prepared"
            );
            return Admission::Refused(KEEPER_UPDATE_WRITE_REFUSAL);
        }
        let lane = self.lane(channel_id, &self.admission);
        lane.waiting.fetch_add(1, Ordering::AcqRel);
        tracing::debug!(
            channel_id = ?channel_id,
            kind = kind.as_str(),
            "a keeper write asked for the write-ordering lane"
        );
        Admission::Granted(AdmissionTicket::new(channel_id, kind, lane))
    }

    /// Record that a keeper replacement is prepared, or that it is not.
    ///
    /// Both edges are logged because the admission gate is invisible from the
    /// outside: a worker that quietly stops accepting terminal writes looks
    /// exactly like a coordinator outage unless the transition is on the record.
    pub fn set_keeper_update_prepared(&self, prepared: bool) {
        self.keeper_update_prepared
            .store(prepared, Ordering::Release);
        tracing::warn!(
            prepared,
            "the keeper replacement gate changed; terminal writes are now {}",
            if prepared { "refused" } else { "admitted" }
        );
    }

    /// Whether a keeper replacement is currently prepared.
    pub fn keeper_update_prepared(&self) -> bool {
        self.keeper_update_prepared.load(Ordering::Acquire)
    }

    /// What a diagnostic report says about one channel's lanes.
    pub fn snapshot(&self, channel_id: ChannelId) -> LaneSnapshot {
        let control = self.state(&self.control, channel_id);
        let admission = self.state(&self.admission, channel_id);
        LaneSnapshot {
            control_depth: control.0,
            control_running: ControlKind::from_report_index(control.1),
            control_running_since_ms: control.2,
            admission_depth: admission.0,
            admission_holder: AdmissionKind::from_report_index(admission.1),
            admission_held_since_ms: admission.2,
        }
    }

    fn lane(
        &self,
        channel_id: ChannelId,
        table: &Mutex<HashMap<ChannelId, Arc<Lane>>>,
    ) -> Arc<Lane> {
        let mut lanes = self.lock(table);
        Arc::clone(lanes.entry(channel_id).or_default())
    }

    /// Drop an idle lane, so a diagnostic reports "idle" rather than a retained
    /// never-cleared record.
    ///
    /// The count is read under the TABLE's lock and the permit is already
    /// dropped, so a caller that has not yet incremented cannot hold this lane —
    /// and one that has is counted, which is what keeps a live lane from being
    /// removed from under a waiter.
    fn forget_if_idle(
        &self,
        channel_id: ChannelId,
        lane: &Arc<Lane>,
        table: &Mutex<HashMap<ChannelId, Arc<Lane>>>,
    ) {
        if lane.waiting.load(Ordering::Acquire) != 0 {
            return;
        }
        let mut lanes = self.lock(table);
        if let Some(current) = lanes.get(&channel_id)
            && Arc::ptr_eq(current, lane)
            && lane.waiting.load(Ordering::Acquire) == 0
        {
            lanes.remove(&channel_id);
        }
    }

    fn state(
        &self,
        table: &Mutex<HashMap<ChannelId, Arc<Lane>>>,
        channel_id: ChannelId,
    ) -> (u32, u32, u64) {
        let lanes = self.lock(table);
        let Some(lane) = lanes.get(&channel_id) else {
            return (0, 0, 0);
        };
        (
            lane.waiting.load(Ordering::Relaxed),
            lane.running.load(Ordering::Relaxed),
            lane.running_since.load(Ordering::Relaxed),
        )
    }

    fn lock<'a, T>(&self, table: &'a Mutex<T>) -> MutexGuard<'a, T> {
        lock(table)
    }
}

/// One channel's lane: the gate its writers queue on, and what a report reads.
#[derive(Debug)]
pub(super) struct Lane {
    /// Shared so a writer can take the permit and hold it while the holder
    /// moves on: the gate outlives whoever admitted into it.
    pub(super) gate: Arc<tokio::sync::Semaphore>,
    /// Writers queued or holding, and a zero here is what makes a lane removable.
    pub(super) waiting: AtomicU32,
    /// The report index of the writer holding it, zero when idle.
    pub(super) running: AtomicU32,
    pub(super) running_since: AtomicU64,
}

impl Default for Lane {
    fn default() -> Self {
        Self {
            gate: Arc::new(tokio::sync::Semaphore::new(1)),
            waiting: AtomicU32::new(0),
            running: AtomicU32::new(0),
            running_since: AtomicU64::new(0),
        }
    }
}

/// What both lanes say about one channel, for a diagnostic report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LaneSnapshot {
    pub control_depth: u32,
    pub control_running: Option<ControlKind>,
    pub control_running_since_ms: u64,
    pub admission_depth: u32,
    pub admission_holder: Option<AdmissionKind>,
    pub admission_held_since_ms: u64,
}

/// Milliseconds since this process started its clock.
///
/// A monotonic reading, never a wall clock: a lane held for an hour has been
/// held for an hour whether or not NTP moved the time under it.
pub(super) fn mono_ms() -> u64 {
    static START: LazyLock<Instant> = LazyLock::new(Instant::now);
    START.elapsed().as_millis() as u64
}

/// A poisoned lane bookkeeping mutex is a bookkeeping fault, not a lost one:
/// the permits it hands out are independent, and a panic while holding it
/// leaves every one of them already released.
pub(super) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
