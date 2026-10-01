//! The approver's second phase: the code is on screen, and this page follows
//! the requester to a decision.
//!
//! A child of the flow module because it reads the parent's private ceremony
//! handle; splitting it out is also what keeps the parent under the file cap.
//!
//! Ports the `awaiting_confirmation` and `cancelling` phases of
//! `apps/web/src/components/pairing/PairApprovalProvider.tsx:210-292` and the
//! evidence rules in `store/auth/pair-approval-lifecycle.ts:92-124`.

use dioxus::prelude::ReadableExt as _;

use roost_client_core::client::auth::PairApproval;
use roost_client_core::client::auth::pairing_requests::PairDenyRequest;
use roost_client_core::client::rpc::calls::pairing::{
    DenyPair, PairApprovalStatus, ReadPairApprovalStatus,
};

use crate::components::pairing::approver::{ApprovalPhase, CodeDialogState};
use crate::components::pairing::failure::{ApprovalFailure, describe, retry_delay_ms};
use crate::components::pairing::notices::NoticeTone;
use crate::components::terminal::dom::sleep_ms;

use super::{ApprovalOutcome, ApproverRig, STATUS_POLL_INTERVAL_MS, StatusRead};

impl ApproverRig {
    /// Follow the requester until the ceremony decides, once a second.
    pub(crate) async fn await_confirmation(&self, approval: &PairApproval, generation: u64) {
        loop {
            sleep_ms(STATUS_POLL_INTERVAL_MS).await;
            if self.is_stale(generation) || self.retired.get() || self.cancelling.get() {
                return;
            }
            match self.read_status(approval).await {
                StatusRead::Answer(PairApprovalStatus::VerificationRequired) => {}
                StatusRead::Answer(status) => {
                    self.apply_status(status);
                    return;
                }
                StatusRead::Retry => {}
                StatusRead::Settle(outcome) => {
                    self.settle(outcome);
                    return;
                }
            }
        }
    }

    /// One `PairApprovalStatus` read.
    async fn read_status(&self, approval: &PairApproval) -> StatusRead {
        let call = ReadPairApprovalStatus {
            request: approval.status_request(),
        };
        match self.pump.rpc().call(&call).await {
            Ok(status) => StatusRead::Answer(status),
            Err(error) => match ApprovalFailure::classify(&error, "PairApprovalStatus") {
                ApprovalFailure::Retry => StatusRead::Retry,
                ApprovalFailure::Gone => StatusRead::Settle(ApprovalOutcome::Unavailable),
                ApprovalFailure::Authority => StatusRead::Settle(ApprovalOutcome::Authority),
                ApprovalFailure::Reload => StatusRead::Settle(ApprovalOutcome::Reload),
            },
        }
    }

    /// Turn a status this client understands into the ceremony's end.
    pub(crate) fn apply_status(&self, status: PairApprovalStatus) {
        let outcome = match status {
            PairApprovalStatus::VerificationRequired => {
                tracing::warn!(target: "auth", "auth.pair_status_unchanged");
                return;
            }
            PairApprovalStatus::Completed => ApprovalOutcome::Completed(
                self.approval
                    .borrow()
                    .as_ref()
                    .map(|approval| approval.requester_label.clone())
                    .unwrap_or_default(),
            ),
            PairApprovalStatus::Denied => ApprovalOutcome::Denied,
            PairApprovalStatus::Expired => ApprovalOutcome::Expired,
            PairApprovalStatus::VerificationFailed => ApprovalOutcome::VerificationFailed,
            PairApprovalStatus::Unknown(name) => {
                tracing::warn!(target: "auth", status = %name, "pair approval status is not one this client knows");
                ApprovalOutcome::Reload
            }
        };
        self.settle(outcome);
    }

    /// Withdraw the open approval, server-side.
    ///
    /// Every dismissal of the code dialog routes here, and the persisted record
    /// is dropped FIRST: it exists only to replay `PairApprove` after a reload,
    /// and once a cancellation is in flight an approval must never be replayed.
    pub async fn cancel(&self) {
        let Some(approval) = self.approval.borrow().clone() else {
            return;
        };
        if self.state.peek().phase != ApprovalPhase::AwaitingConfirmation {
            return;
        }
        self.ceremony().clear_approval();
        self.retired.set(true);
        self.cancelling.set(true);
        self.patch(|state| {
            state.phase = ApprovalPhase::Cancelling;
            state.dialog = CodeDialogState::Cancelling;
        });
        self.submit_cancellation(&approval).await;
        self.cancelling.set(false);
    }

    /// `PairDeny` until the coordinator agrees or says the request is gone.
    async fn submit_cancellation(&self, approval: &PairApproval) {
        let mut attempt = 0;
        loop {
            let call = DenyPair {
                request: approval.deny_request(),
            };
            match self.pump.rpc().call(&call).await {
                Ok(_) => {
                    tracing::info!(target: "auth", "auth.pair_denied");
                    self.settle(ApprovalOutcome::Cancelled);
                    return;
                }
                Err(error) => match ApprovalFailure::classify(&error, "PairDeny") {
                    ApprovalFailure::Retry => {
                        let delay = retry_delay_ms(attempt);
                        attempt = attempt.saturating_add(1);
                        sleep_ms(delay).await;
                    }
                    // `NotFound`: the row already left the live states, possibly
                    // through this very denial whose answer was lost, so the
                    // status read decides — and if the requester is still
                    // waiting, the denial is sent again.
                    ApprovalFailure::Gone => {
                        if !self.resend_after_gone(approval).await {
                            return;
                        }
                    }
                    ApprovalFailure::Authority => {
                        self.settle(ApprovalOutcome::Authority);
                        return;
                    }
                    ApprovalFailure::Reload => {
                        self.settle(ApprovalOutcome::Reload);
                        return;
                    }
                },
            }
        }
    }

    /// Read the status after a denial that reported the row missing.
    ///
    /// `true` means the requester is still waiting, so the denial must be sent
    /// again; `false` means the read already settled the ceremony.
    async fn resend_after_gone(&self, approval: &PairApproval) -> bool {
        match self.read_status(approval).await {
            StatusRead::Answer(PairApprovalStatus::VerificationRequired) => true,
            StatusRead::Answer(status) => {
                self.apply_status(status);
                false
            }
            StatusRead::Settle(outcome) => {
                self.settle(outcome);
                false
            }
            // The status read is transiently unavailable, which says nothing
            // about the request. Send the denial again: it is idempotent on a
            // row that is already gone, and it is the only way this client stops
            // being the reason a live code sits unanswered.
            StatusRead::Retry => true,
        }
    }

    /// Refuse a request without ever generating a code for it.
    pub async fn deny(&self, ephemeral_id: &str) {
        let call = DenyPair {
            request: PairDenyRequest {
                ephemeral_id: ephemeral_id.to_string(),
            },
        };
        match self.pump.rpc().call(&call).await {
            Ok(_) => {
                // The coordinator agreed, so nothing can answer this request
                // any more and the row leaves the list. Dispatched, not
                // filtered: one removal, one reason.
                self.dismiss(ephemeral_id);
                tracing::info!(target: "auth", ephemeral_id, "pair request denied");
                self.patch(|state| {
                    state.notice = Some((NoticeTone::Ok, "Denied".to_string()));
                });
            }
            Err(error) => {
                tracing::warn!(target: "auth", %error, "pair denial refused");
                let message = format!("Deny failed: {}", describe(&error));
                self.patch(|state| state.notice = Some((NoticeTone::Error, message)));
            }
        }
    }

    /// End the ceremony, and say how.
    ///
    /// `Reload` is the one outcome that keeps the code on screen: the code stays
    /// valid for the requester, and this page keeps "Cancel request" available
    /// rather than pretending the ceremony never existed.
    pub(crate) fn settle(&self, outcome: ApprovalOutcome) {
        self.cancelling.set(false);
        if outcome == ApprovalOutcome::Reload {
            self.patch(|state| {
                state.dialog = CodeDialogState::ReloadRequired;
                state.phase = ApprovalPhase::AwaitingConfirmation;
            });
            return;
        }
        self.retired.set(true);
        self.ceremony().clear_approval();
        *self.approval.borrow_mut() = None;
        let notice = outcome.notice();
        self.patch(|state| {
            state.phase = ApprovalPhase::Idle;
            state.approval = None;
            state.busy_request_id = None;
            state.notice = notice;
        });
        tracing::info!(target: "auth", ?outcome, "auth.pair_approval_settled");
    }
}
