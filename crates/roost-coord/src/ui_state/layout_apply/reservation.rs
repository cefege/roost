//! The owner's reservations: registering a live target, publishing an apply
//! against it, admitting exactly one result, and retiring what nobody answers.
//! Split from the parent module so the types and the ledger's behaviour stay
//! readable side by side; both are one owner and one ledger.

use std::sync::atomic::Ordering;
use std::sync::{Arc, MutexGuard};

use roost_proto::{UiApplyLayoutOutcome, UiApplyLayoutResult};
use tokio::sync::oneshot;

use super::{
    LayoutApplyLedger, LayoutApplyOwnerStats, LayoutApplyRequest, LayoutApplyTargetGuard,
    PendingLayoutApply, PendingReservation, UI_LAYOUT_APPLY_MAX_PENDING,
    UI_LAYOUT_APPLY_TIMEOUT_MS, UiLayoutApplyCapacityError, UiLayoutApplyOwner,
    UiLayoutApplyPublication, UiLayoutApplyResolution, UiLayoutApplyTarget, target_gone,
};
use crate::ui_state::limits::{UI_STATE_MAX_TABS_PER_FINGERPRINT, UI_STATE_MAX_TABS_TOTAL};
use crate::ui_state::rejected_reason::sanitized_rejected_reason;

impl UiLayoutApplyOwner {
    /// An owner over the real clock, with v2's timeout and bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_clock(Arc::new(crate::serve::now_ms))
    }

    /// An owner whose clock the caller supplies, so a test never waits for this
    /// one to tick.
    #[must_use]
    pub fn with_clock(now_ms: Arc<dyn Fn() -> i64 + Send + Sync>) -> Self {
        Self {
            ledger: Arc::new(std::sync::Mutex::new(LayoutApplyLedger::default())),
            now_ms,
            timeout_ms: UI_LAYOUT_APPLY_TIMEOUT_MS,
            max_pending: UI_LAYOUT_APPLY_MAX_PENDING,
            max_targets_per_fingerprint: UI_STATE_MAX_TABS_PER_FINGERPRINT,
            max_targets_total: UI_STATE_MAX_TABS_TOTAL,
            next_correlation: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    /// Register the exact live socket a tab may be sent an apply on.
    ///
    /// Registering an already-registered `(fingerprint, tab id)` REPLACES the
    /// socket, which is the redial case: the previous generation's reservations
    /// are settled as target-gone, because that socket can no longer answer for
    /// the tab and letting them sit would hold the caller's request open.
    pub fn register_target(
        &self,
        target: UiLayoutApplyTarget,
    ) -> Result<LayoutApplyTargetGuard, UiLayoutApplyCapacityError> {
        let key = target.registration_key();
        self.expire_due();
        let mut ledger = self.lock();
        let is_new = !ledger.targets.contains_key(&key);
        if is_new {
            let held = ledger
                .target_counts_by_fingerprint
                .get(&target.fingerprint)
                .copied()
                .unwrap_or(0);
            if held >= self.max_targets_per_fingerprint
                || ledger.targets.len() >= self.max_targets_total
            {
                return Err(UiLayoutApplyCapacityError);
            }
            ledger
                .target_counts_by_fingerprint
                .insert(target.fingerprint.clone(), held + 1);
        }
        let replaced = ledger.targets.insert(key, target.clone());
        if let Some(previous) = replaced {
            self.settle_target_gone_for(&mut ledger, &previous, "replaced");
        }
        Ok(LayoutApplyTargetGuard {
            owner: self.clone(),
            target,
        })
    }

    /// Reserve the named device's exact live tab socket and publish once.
    ///
    /// The reservation is in the ledger BEFORE `publish` runs, so a browser that
    /// answers synchronously -- a subscriber that settles the result inline,
    /// which is what the tests do -- still finds its correlation.
    pub fn request_apply<F>(
        &self,
        target_fingerprint: &str,
        target_tab_id: &str,
        publish: F,
    ) -> Result<LayoutApplyRequest, UiLayoutApplyCapacityError>
    where
        F: FnOnce(&UiLayoutApplyPublication),
    {
        self.expire_due();
        let correlation_id = self.allocate_correlation_id();
        let (publication, receiver) = {
            let mut ledger = self.lock();
            let key = (target_fingerprint.to_owned(), target_tab_id.to_owned());
            let Some(target) = ledger.targets.get(&key).cloned() else {
                return Ok(LayoutApplyRequest::TargetGone(target_gone(correlation_id)));
            };
            if ledger.pending.len() >= self.max_pending {
                return Err(UiLayoutApplyCapacityError);
            }
            let (responder, receiver) = oneshot::channel();
            ledger.pending.insert(
                correlation_id.clone(),
                PendingReservation {
                    target: target.clone(),
                    deadline_ms: (self.now_ms)() + self.timeout_ms,
                    responder,
                },
            );
            (
                UiLayoutApplyPublication {
                    target,
                    correlation_id: correlation_id.clone(),
                },
                receiver,
            )
        };
        publish(&publication);
        Ok(LayoutApplyRequest::Pending(PendingLayoutApply {
            owner: self.clone(),
            correlation_id,
            receiver: Some(receiver),
        }))
    }

    /// Accept a browser's answer, and only on the fence it was reserved with.
    ///
    /// Every one of these must hold, and each is a fence a port could drop: the
    /// outcome is one a browser may prove, the correlation is one this owner
    /// issued, the pending was reserved ON that exact fingerprint, tab and
    /// socket, and that socket is still the registered target. A reservation is
    /// removed on the first acceptance, so a duplicate answer -- a replayed
    /// frame, or a browser that answers twice -- settles nothing and applies
    /// nothing.
    pub fn accept_result(
        &self,
        source: &UiLayoutApplyTarget,
        result: &UiApplyLayoutResult,
    ) -> bool {
        let Some(outcome) = result.outcome.as_known() else {
            return false;
        };
        if !matches!(
            outcome,
            UiApplyLayoutOutcome::Applied | UiApplyLayoutOutcome::Rejected
        ) {
            return false;
        }
        self.expire_due();
        let mut ledger = self.lock();
        let Some(pending) = ledger.pending.get(&result.correlation_id) else {
            return false;
        };
        if &pending.target != source {
            return false;
        }
        let key = source.registration_key();
        if ledger.targets.get(&key).map(|current| &current.socket_id) != Some(&source.socket_id) {
            return false;
        }
        let Some(pending) = ledger.pending.remove(&result.correlation_id) else {
            return false;
        };
        let resolution = match outcome {
            UiApplyLayoutOutcome::Applied => UiLayoutApplyResolution {
                outcome,
                correlation_id: result.correlation_id.clone(),
                reason: None,
            },
            _ => UiLayoutApplyResolution {
                outcome,
                correlation_id: result.correlation_id.clone(),
                reason: Some(sanitized_rejected_reason(result.reason.as_deref())),
            },
        };
        let _ = pending.responder.send(resolution);
        true
    }

    /// The owner's depth, for a log line and for tests.
    #[must_use]
    pub fn stats(&self) -> LayoutApplyOwnerStats {
        let ledger = self.lock();
        LayoutApplyOwnerStats {
            targets: ledger.targets.len(),
            pending: ledger.pending.len(),
        }
    }

    /// Settle every outstanding apply as target-gone and forget every target.
    pub fn dispose(&self) {
        let mut ledger = self.lock();
        ledger.targets.clear();
        ledger.target_counts_by_fingerprint.clear();
        let correlation_ids: Vec<String> = ledger.pending.keys().cloned().collect();
        for correlation_id in correlation_ids {
            if let Some(pending) = ledger.pending.remove(&correlation_id) {
                let _ = pending.responder.send(target_gone(correlation_id));
            }
        }
    }

    pub(super) fn unregister_target(&self, target: &UiLayoutApplyTarget) {
        self.expire_due();
        let key = target.registration_key();
        let mut ledger = self.lock();
        if ledger.targets.get(&key) != Some(target) {
            return;
        }
        ledger.targets.remove(&key);
        let held = ledger
            .target_counts_by_fingerprint
            .get(&target.fingerprint)
            .copied()
            .unwrap_or(0);
        if held <= 1 {
            ledger
                .target_counts_by_fingerprint
                .remove(&target.fingerprint);
        } else {
            ledger
                .target_counts_by_fingerprint
                .insert(target.fingerprint.clone(), held - 1);
        }
        self.settle_target_gone_for(&mut ledger, target, "closed");
    }

    /// Retire every reservation whose deadline has passed.
    ///
    /// v2 arms one timer per reservation; here the deadline is a timestamp and
    /// every entry point reaps first, so a reservation whose awaiting task was
    /// dropped -- or whose owner was never reached again -- cannot outlive its
    /// timeout by more than one call.
    fn expire_due(&self) {
        let now = (self.now_ms)();
        let mut ledger = self.lock();
        let expired: Vec<String> = ledger
            .pending
            .iter()
            .filter(|(_, pending)| now >= pending.deadline_ms)
            .map(|(correlation_id, _)| correlation_id.clone())
            .collect();
        for correlation_id in expired {
            if let Some(pending) = ledger.pending.remove(&correlation_id) {
                let _ = pending.responder.send(target_gone(correlation_id));
            }
        }
    }

    pub(super) fn expire_pending(&self, correlation_id: &str) -> Option<UiLayoutApplyResolution> {
        let mut ledger = self.lock();
        let pending = ledger.pending.remove(correlation_id)?;
        let resolution = target_gone(correlation_id.to_owned());
        let _ = pending.responder.send(resolution.clone());
        Some(resolution)
    }

    pub(super) fn discard_pending(&self, correlation_id: &str) {
        self.lock().pending.remove(correlation_id);
    }

    pub(super) fn remaining_ms(&self, correlation_id: &str) -> u64 {
        let ledger = self.lock();
        let Some(pending) = ledger.pending.get(correlation_id) else {
            return 0;
        };
        u64::try_from(pending.deadline_ms - (self.now_ms)()).unwrap_or(0)
    }

    /// Settle every reservation made against one exact socket generation.
    fn settle_target_gone_for(
        &self,
        ledger: &mut LayoutApplyLedger,
        target: &UiLayoutApplyTarget,
        cause: &'static str,
    ) {
        let correlation_ids: Vec<String> = ledger
            .pending
            .iter()
            .filter(|(_, pending)| &pending.target == target)
            .map(|(correlation_id, _)| correlation_id.clone())
            .collect();
        for correlation_id in correlation_ids {
            if let Some(pending) = ledger.pending.remove(&correlation_id) {
                tracing::debug!(
                    event = "ui-layout-apply",
                    action = "pending_target_gone",
                    correlation_id = correlation_id,
                    socket_id = target.socket_id,
                    cause = cause,
                    "a reserved layout apply had no socket left to answer on it"
                );
                let _ = pending.responder.send(target_gone(correlation_id));
            }
        }
    }

    fn allocate_correlation_id(&self) -> String {
        let ordinal = self.next_correlation.fetch_add(1, Ordering::Relaxed);
        format!("ui-layout-apply-{ordinal}")
    }

    fn lock(&self) -> MutexGuard<'_, LayoutApplyLedger> {
        // A poisoned lock means a publisher panicked mid-reservation. Every
        // mutation here removes or replaces a whole entry, so recovering the
        // guard loses nothing that was half-written, and refusing every later
        // layout apply for the life of the process would be a worse answer.
        self.ledger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl Default for UiLayoutApplyOwner {
    fn default() -> Self {
        Self::new()
    }
}
