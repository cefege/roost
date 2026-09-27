//! Live writable tab targets and the acknowledged layout applies reserved on them.
//!
//! Ported from `apps/coord/src/ui-state/ui-layout-apply-owner.ts`. The UI RPC
//! registers a pending correlation BEFORE it publishes, and Sync ingress settles
//! it only through the exact fingerprint, tab and socket generation -- so a
//! browser that redials cannot acknowledge an apply meant for its predecessor.
//! `sync_ws::CommandOutcome::LayoutResult` is the only path in.

mod reservation;

use roost_proto::UiApplyLayoutOutcome;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

/// How long a reserved apply waits for its target's acknowledgement.
///
/// 15 seconds: long enough for a background browser tab to wake and paint, short
/// enough that a caller is not left holding a request that will never answer.
pub const UI_LAYOUT_APPLY_TIMEOUT_MS: i64 = 15_000;

/// How many applies may be awaiting acknowledgement at once, process-wide.
pub const UI_LAYOUT_APPLY_MAX_PENDING: usize = 256;

/// The reason a caller is given when its target has no live socket to answer on.
pub const UI_LAYOUT_TARGET_GONE_REASON: &str = "target acknowledgement unavailable";

/// The one live writable tab socket an apply may be reserved on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiLayoutApplyTarget {
    /// The browser key fingerprint the socket authenticated as.
    pub fingerprint: String,
    /// The tab id the socket speaks for.
    pub tab_id: String,
    /// The coordinator-minted socket generation.
    pub socket_id: String,
}

impl UiLayoutApplyTarget {
    /// The identity a target is registered and reserved under.
    fn registration_key(&self) -> (String, String) {
        (self.fingerprint.clone(), self.tab_id.clone())
    }
}

/// What a caller is told about its apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiLayoutApplyResolution {
    /// Applied, rejected, or the target was gone.
    pub outcome: UiApplyLayoutOutcome,
    /// The correlation the caller may quote in a log line.
    pub correlation_id: String,
    /// Why, for a rejection or a target that could not answer.
    pub reason: Option<String>,
}

fn target_gone(correlation_id: String) -> UiLayoutApplyResolution {
    UiLayoutApplyResolution {
        outcome: UiApplyLayoutOutcome::TargetGone,
        correlation_id,
        reason: Some(UI_LAYOUT_TARGET_GONE_REASON.to_owned()),
    }
}

/// Why an apply was refused before it was published.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UiLayoutApplyCapacityError;

impl UiLayoutApplyCapacityError {
    /// The message a caller reads.
    #[must_use]
    pub fn message(self) -> &'static str {
        "ui layout apply capacity exhausted"
    }
}

/// A reserved apply, as it goes onto the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiLayoutApplyPublication {
    /// The exact socket the apply was reserved on.
    pub target: UiLayoutApplyTarget,
    /// The id the target's acknowledgement must quote.
    pub correlation_id: String,
}

/// A reserved apply awaiting its target's acknowledgement.
pub struct PendingLayoutApply {
    owner: UiLayoutApplyOwner,
    correlation_id: String,
    receiver: Option<oneshot::Receiver<UiLayoutApplyResolution>>,
}

impl std::fmt::Debug for PendingLayoutApply {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingLayoutApply")
            .field("correlation_id", &self.correlation_id)
            .finish()
    }
}

impl PendingLayoutApply {
    /// The correlation this apply was published under.
    #[must_use]
    pub fn correlation_id(&self) -> &str {
        &self.correlation_id
    }

    /// Wait for the target's answer, or for the reservation to time out.
    ///
    /// A timeout is a real answer here, not an error: the caller gets
    /// `TargetGone` and the reservation is retired, which is the same thing a
    /// closed socket produces.
    pub async fn await_resolution(mut self) -> UiLayoutApplyResolution {
        let receiver = self.receiver.take().unwrap_or_else(|| oneshot::channel().1);
        let remaining = self.owner.remaining_ms(&self.correlation_id);
        match tokio::time::timeout(Duration::from_millis(remaining), receiver).await {
            Ok(Ok(resolution)) => resolution,
            Ok(Err(_)) => target_gone(self.correlation_id.clone()),
            Err(_) => self
                .owner
                .expire_pending(&self.correlation_id)
                .unwrap_or_else(|| target_gone(self.correlation_id.clone())),
        }
    }
}

impl Drop for PendingLayoutApply {
    /// A dropped await means the caller is gone, so the reservation is retired
    /// rather than left for the timeout to find.
    fn drop(&mut self) {
        if self.receiver.is_some() {
            self.owner.discard_pending(&self.correlation_id);
        }
    }
}

/// What a reservation produced: either a live apply or an immediate answer.
#[derive(Debug)]
pub enum LayoutApplyRequest {
    /// The target had no live socket. Nothing was published.
    TargetGone(UiLayoutApplyResolution),
    /// Reserved and published; await the acknowledgement.
    Pending(PendingLayoutApply),
}

/// A registered target's slot, released when the guard drops.
#[derive(Debug)]
pub struct LayoutApplyTargetGuard {
    owner: UiLayoutApplyOwner,
    target: UiLayoutApplyTarget,
}

impl Drop for LayoutApplyTargetGuard {
    /// A disposer for a target that has already been REPLACED must not remove
    /// the replacement: the replacement's socket is the live one, and dropping
    /// the old guard is exactly the case where a colliding tab id would
    /// otherwise take the victim's target with it.
    fn drop(&mut self) {
        self.owner.unregister_target(&self.target);
    }
}

/// What a log line or a test reads about the owner's depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutApplyOwnerStats {
    /// Registered live targets.
    pub targets: usize,
    /// Applies awaiting acknowledgement.
    pub pending: usize,
}

struct PendingReservation {
    target: UiLayoutApplyTarget,
    deadline_ms: i64,
    responder: oneshot::Sender<UiLayoutApplyResolution>,
}

#[derive(Default)]
struct LayoutApplyLedger {
    targets: BTreeMap<(String, String), UiLayoutApplyTarget>,
    target_counts_by_fingerprint: BTreeMap<String, usize>,
    pending: BTreeMap<String, PendingReservation>,
}

/// Every live writable tab target and its reserved layout applies.
#[derive(Clone)]
pub struct UiLayoutApplyOwner {
    ledger: Arc<Mutex<LayoutApplyLedger>>,
    now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    timeout_ms: i64,
    max_pending: usize,
    max_targets_per_fingerprint: usize,
    max_targets_total: usize,
    next_correlation: Arc<AtomicU64>,
}

impl std::fmt::Debug for UiLayoutApplyOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UiLayoutApplyOwner")
            .field("stats", &self.stats())
            .finish()
    }
}
