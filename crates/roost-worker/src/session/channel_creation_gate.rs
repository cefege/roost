//! Fences keeper-channel creation against keeper replacement. Ports v2
//! `apps/worker/src/session/session-channel-creation-gate.ts` and the
//! `SessionManager` half that uses it (`session-manager.ts:44-74`): every spawn
//! or respawn holds one lease for its whole creation; update preparation closes
//! admission synchronously, then waits for every lease already in flight.
//! Called by `session::respawn` (leases) and the keeper-update owner (prepare).
//!
//! THE SAME PREPARATION FREEZES TERMINAL WRITES. v2's `keeperUpdatePrepared` is
//! `gate.blocksTerminalWrites()`; here the gate drives
//! [`super::control_lanes::ControlLanes::set_keeper_update_prepared`] on each
//! 0↔1 edge of the preparation count, inside the count's own lock, so the flag
//! the write lane reads can never disagree with the count.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::watch;

use super::control_lanes::ControlLanes;
use super::lifecycle::SessionManager;
use crate::browser_commands::Refusal;

/// What a creation refused by an open preparation is answered with (v2).
pub const CHANNEL_CREATION_REFUSAL: &str =
    "worker keeper update preparation blocks channel creation";

/// The two counts, updated together under the watch channel's lock.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct GateCounts {
    active_creations: usize,
    preparations: usize,
}

#[derive(Debug)]
struct GateState {
    counts: watch::Sender<GateCounts>,
    lanes: Arc<ControlLanes>,
}

/// v2 `SessionChannelCreationGate`.
#[derive(Debug, Clone)]
pub struct ChannelCreationGate {
    state: Arc<GateState>,
}

impl ChannelCreationGate {
    /// A gate with no creation and no preparation, driving `lanes`' write freeze.
    pub fn new(lanes: Arc<ControlLanes>) -> Self {
        Self {
            state: Arc::new(GateState {
                counts: watch::Sender::new(GateCounts::default()),
                lanes,
            }),
        }
    }

    /// Whether a keeper-update preparation is open.
    pub fn preparation_active(&self) -> bool {
        self.state.counts.borrow().preparations > 0
    }

    /// Keeper replacement is imminent while preparation is open, so a terminal
    /// write must fail before it reaches the keeper the update is about to
    /// replace: a pre-write refusal is retryable, an absorbed write is not.
    pub fn blocks_terminal_writes(&self) -> bool {
        self.preparation_active()
    }

    /// One creation lease, or `None` while a preparation is open.
    pub fn try_acquire(&self) -> Option<CreationLease> {
        let mut acquired = false;
        self.state.counts.send_if_modified(|counts| {
            if counts.preparations > 0 {
                return false;
            }
            counts.active_creations += 1;
            acquired = true;
            true
        });
        acquired.then(|| CreationLease {
            state: Arc::clone(&self.state),
        })
    }

    /// Close admission NOW, then wait for every creation that already held a
    /// lease. The count moves before this returns; the future only waits.
    pub fn begin_preparation(&self) -> impl Future<Output = PreparationRollback> + Send + 'static {
        let lanes = Arc::clone(&self.state.lanes);
        self.state.counts.send_modify(|counts| {
            counts.preparations += 1;
            if counts.preparations == 1 {
                lanes.set_keeper_update_prepared(true);
            }
        });
        let in_flight = self.state.counts.borrow().active_creations;
        tracing::warn!(
            in_flight,
            "keeper update preparation closed channel creation"
        );
        let mut drained = self.state.counts.subscribe();
        let state = Arc::clone(&self.state);
        async move {
            if drained
                .wait_for(|counts| counts.active_creations == 0)
                .await
                .is_err()
            {
                tracing::error!("the channel creation gate closed while a preparation waited");
            }
            tracing::info!("keeper update preparation drained every in-flight channel creation");
            PreparationRollback {
                state,
                rolled_back: AtomicBool::new(false),
            }
        }
    }
}

/// One creation's hold on the gate. Released when dropped, exactly once.
#[derive(Debug)]
pub struct CreationLease {
    state: Arc<GateState>,
}

impl Drop for CreationLease {
    fn drop(&mut self) {
        self.state.counts.send_modify(|counts| {
            counts.active_creations = counts.active_creations.saturating_sub(1);
        });
    }
}

/// Reopens admission for ONE preparation attempt. Explicit, never on drop: a
/// preparation that succeeded keeps creation closed until the keeper it fenced
/// is gone (v2's gate stays closed on success).
#[derive(Debug)]
pub struct PreparationRollback {
    state: Arc<GateState>,
    rolled_back: AtomicBool,
}

impl PreparationRollback {
    /// Release this preparation. Idempotent: a second call changes nothing.
    pub fn rollback(&self) {
        if self.rolled_back.swap(true, Ordering::AcqRel) {
            return;
        }
        let lanes = Arc::clone(&self.state.lanes);
        self.state.counts.send_modify(|counts| {
            counts.preparations = counts.preparations.saturating_sub(1);
            if counts.preparations == 0 {
                lanes.set_keeper_update_prepared(false);
            }
        });
        tracing::warn!("a keeper update preparation was rolled back");
    }
}

impl SessionManager {
    /// Keeper replacement preparation: creation and terminal writes fail closed
    /// (v2 `keeperUpdatePrepared`).
    pub fn keeper_update_prepared(&self) -> bool {
        self.creation_gate.blocks_terminal_writes()
    }

    /// Close channel creation synchronously, then wait for every creation that
    /// already held a lease. Rollback releases only this preparation attempt.
    pub fn begin_keeper_update_preparation(
        &self,
    ) -> impl Future<Output = PreparationRollback> + Send + 'static {
        self.creation_gate.begin_preparation()
    }

    /// The lease a spawn or respawn holds for its whole creation (v2
    /// `#executeAdmittedChannelCreation`), or the refusal while a keeper
    /// update is being prepared.
    pub(super) fn admit_channel_creation(&self) -> Result<CreationLease, Refusal> {
        self.creation_gate.try_acquire().ok_or_else(|| {
            tracing::warn!(
                "a channel creation was refused because a keeper update is being prepared"
            );
            Refusal::failed("sessions", CHANNEL_CREATION_REFUSAL)
        })
    }
}
