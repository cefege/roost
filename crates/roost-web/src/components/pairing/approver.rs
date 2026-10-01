//! The approver's side of one request: the code it generated, the dialog that
//! shows it, and the status loop that retires it.
//!
//! Ported from `apps/web/src/components/pairing/PairApprovalProvider.tsx` and
//! the evidence classification it delegates to
//! (`store/auth/pair-approval-lifecycle.ts`). The split v2 already draws is
//! kept: this file owns the signals and the fences, `flow` owns the timers,
//! and `pairing::failure` owns what a refusal means.
//!
//! CLOSING THE DIALOG IS A SERVER-SIDE CANCELLATION. Every dismissal — the
//! close button, Escape, the backdrop, "Cancel request" — routes to
//! [`PairApprover::cancel`], which issues `PairDeny`. An approval a human walked
//! away from must not sit on the coordinator holding a live six-digit code, and
//! the requester polling it is entitled to a `denied` rather than to a request
//! that has to time out on its own. `PairDeny` on a row that has already left
//! the live states answers `NotFound`, and the follow-up `PairApprovalStatus`
//! read is what says what actually happened.

mod flow;

use std::cell::Cell;

use dioxus::prelude::*;
use roost_client_core::client::auth::PairApproval;

// The rig is this module's half; the outcome and the poll cadence belong to
// `flow` and to the flow's own children, and re-exporting them here would
// advertise a second place to reach them from.
pub use flow::ApproverRig;

use super::notices::NoticeTone;
use crate::pump::{Pump, use_pump};

/// Where an approval is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPhase {
    /// No approval in flight; the list is answerable.
    Idle,
    /// `PairApprove` is in flight, or retrying.
    Approving,
    /// The code is on screen and the requester has not proved it.
    AwaitingConfirmation,
    /// A dismissal is issuing `PairDeny`.
    Cancelling,
}

/// What the code dialog is showing.
///
/// `ReloadRequired` is not a failure of the ceremony: the code stays valid for
/// the requester, and only this page lost the ability to follow it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeDialogState {
    /// The requester may still confirm.
    Awaiting,
    /// The denial is in flight.
    Cancelling,
    /// This client can no longer track the ceremony.
    ReloadRequired,
}

/// Everything the approval list and the dialog draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApproverState {
    /// Where the approval is.
    pub phase: ApprovalPhase,
    /// The generated code's record, while a dialog is open.
    pub approval: Option<PairApproval>,
    /// What the dialog is showing.
    pub dialog: CodeDialogState,
    /// The request one approval is in flight for, so its card can disable.
    pub busy_request_id: Option<String>,
    /// The settled outcome, once there is one to report.
    pub notice: Option<(NoticeTone, String)>,
}

impl ApproverState {
    /// No approval in flight.
    pub fn idle() -> Self {
        Self {
            phase: ApprovalPhase::Idle,
            approval: None,
            dialog: CodeDialogState::Awaiting,
            busy_request_id: None,
            notice: None,
        }
    }
}

/// The one approval an authorized page owns.
#[derive(Clone)]
pub struct PairApprover {
    state: Signal<ApproverState>,
    rig: ApproverRig,
    cancel_now: EventHandler<()>,
    deny_now: EventHandler<String>,
}

/// Two approvers are the same approval when they own the same state signal.
///
/// `#[component]` derives props equality, and an approval list that took the
/// approver by value would otherwise re-render every card on every parent
/// render.
impl PartialEq for PairApprover {
    fn eq(&self, other: &Self) -> bool {
        self.state == other.state
    }
}

/// The approver for the page that is rendering now.
pub fn use_pair_approver() -> PairApprover {
    let pump: Pump = use_pump();
    let state = use_signal(ApproverState::idle);
    // Generation zero is the mount, which restores a persisted approval and
    // never mints one; every later generation is one reader clicking Approve.
    let generation = use_signal(|| 0_u64);
    let rig = use_hook(|| ApproverRig::new(pump, state, generation));
    let cancel_now = use_callback({
        let rig = rig.clone();
        move |()| {
            let rig = rig.clone();
            spawn(async move { rig.cancel().await });
        }
    });
    let deny_now = use_callback({
        let rig = rig.clone();
        move |ephemeral_id: String| {
            let rig = rig.clone();
            spawn(async move { rig.deny(&ephemeral_id).await });
        }
    });
    start_approval_flow(rig.clone(), generation);
    PairApprover {
        state,
        rig,
        cancel_now,
        deny_now,
    }
}

/// Run the approval flow, and re-run it when a newer approval supersedes it.
///
/// `use_future` spawns ONCE and never re-reads its closure, so the reactive half
/// is this effect: approving a request advances the generation, which retires
/// the mount run and starts the one that owns the new code. The generation it
/// last started is remembered so the mount run is not restarted on top of
/// itself.
fn start_approval_flow(rig: ApproverRig, generation: Signal<u64>) {
    let mut approval = use_future({
        let rig = rig.clone();
        move || {
            let mine = *generation.read();
            let rig = rig.clone();
            async move { flow::drive(rig, mine).await }
        }
    });
    let started = use_hook(|| Cell::new(*generation.peek()));
    use_effect(move || {
        let mine = *generation.read();
        if started.get() == mine {
            return;
        }
        started.set(mine);
        approval.restart();
    });
}

impl PairApprover {
    /// The list's and the dialog's state, subscribed for this render.
    pub fn state(&self) -> Signal<ApproverState> {
        self.state
    }

    /// Approve `ephemeral_id`, generating the code and binding it.
    pub fn approve(&self, ephemeral_id: &str, requester_label: &str, expires_at_ms: i64) {
        self.rig
            .approve(ephemeral_id, requester_label, expires_at_ms);
    }

    /// Refuse a request without ever generating a code for it.
    pub fn deny(&self, ephemeral_id: &str) {
        self.deny_now.call(ephemeral_id.to_string());
    }

    /// Withdraw the open approval, server-side. Every dismissal of the code
    /// dialog routes here, and nothing else closes it.
    pub fn cancel(&self) {
        self.cancel_now.call(());
    }
}
