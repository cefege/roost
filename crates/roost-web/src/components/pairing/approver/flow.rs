//! The approver's timers, fences and evidence handling around one `PairApproval`.
//!
//! Owned by the `PairApprover` hook, which is the only caller. `PairApproval`
//! owns the generated code and builds the three request bodies; this owns when
//! to send them and what each answer means. Split out because the two halves
//! change for different reasons, and a flow that also rendered the dialog would
//! be one file over the cap. The requester-confirmation half is a child module
//! because it reads this file's private ceremony handle.
//!
//! Ports `apps/web/src/components/pairing/PairApprovalProvider.tsx:64-345`.

mod confirm;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::ClientEvent;
use roost_client_core::client::auth::{CeremonyStore, PairApproval};
use roost_client_core::client::rpc::calls::pairing::{ApprovePair, PairApprovalStatus};
use roost_client_core::store::shell_intent::ShellIntent;

use crate::components::pairing::failure::{is_transient, retry_delay_ms};
use crate::components::pairing::notices::NoticeTone;
use crate::components::terminal::dom::{now_ms, sleep_ms};
use crate::platform::secrets::BrowserRandomSource;
use crate::platform::storage::SessionStorageKeyValueStore;
use crate::pump::Pump;

use super::{ApprovalPhase, ApproverState, CodeDialogState};

/// How often an approver asks what became of the request it approved.
pub const STATUS_POLL_INTERVAL_MS: u64 = 1_000;

/// How an approval ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The approver withdrew it.
    Cancelled,
    /// The requester proved the code.
    Completed(String),
    /// Somebody else refused the request.
    Denied,
    /// The request timed out.
    Expired,
    /// The requester used the wrong code too many times.
    VerificationFailed,
    /// The request is no longer in the live states at all.
    Unavailable,
    /// This client is no longer a principal the ceremony answers to.
    Authority,
    /// The coordinator said something this client cannot interpret. The code
    /// stays valid for the requester; only this page lost the thread.
    Reload,
    /// `PairApprove` was considered and refused.
    Refused,
}

impl ApprovalOutcome {
    /// What the page says, or `None` for the two outcomes that are not a
    /// sentence: a completion the Sync fold announces, and a reload this page
    /// answers with a dialog state instead.
    pub fn notice(&self) -> Option<(NoticeTone, String)> {
        let (tone, message) = match self {
            Self::Cancelled => (NoticeTone::Ok, "Pairing request cancelled.".to_string()),
            // The Sync `Pair` domain announces a completed pairing exactly once
            // per request id (`handle_sync/fold_controls.rs:85-111`). Saying it
            // here as well would show the reader two announcements for one
            // event, and the specs count them.
            Self::Completed(_) => return None,
            Self::Denied => (NoticeTone::Warn, "Pairing request was denied.".to_string()),
            Self::Expired => (NoticeTone::Warn, "Pair request expired.".to_string()),
            Self::VerificationFailed => {
                (NoticeTone::Warn, "Pairing verification failed.".to_string())
            }
            Self::Unavailable => (
                NoticeTone::Warn,
                "Pairing request is no longer available.".to_string(),
            ),
            Self::Authority => (
                NoticeTone::Error,
                "Pairing authority is no longer valid.".to_string(),
            ),
            Self::Refused => (NoticeTone::Error, "Pair approval was rejected.".to_string()),
            Self::Reload => return None,
        };
        Some((tone, message))
    }
}

/// What one read of the coordinator's answer established.
pub(crate) enum StatusRead {
    /// A status this client understands.
    Answer(PairApprovalStatus),
    /// Worth another attempt, keeping the code.
    Retry,
    /// An outcome that ends the ceremony.
    Settle(ApprovalOutcome),
}

/// The approver's mutable half, shared between the render and the loops.
#[derive(Clone)]
pub struct ApproverRig {
    pub(crate) pump: Pump,
    pub(crate) approval: Rc<RefCell<Option<PairApproval>>>,
    pub(crate) storage: Rc<SessionStorageKeyValueStore>,
    pub(crate) state: Signal<ApproverState>,
    generation: Signal<u64>,
    /// Set while a `PairDeny` is in flight, so the status loop stands aside
    /// rather than racing the call that is about to end the ceremony.
    pub(crate) cancelling: Rc<Cell<bool>>,
    /// Set once the approval must never be replayed. Cleared only by a new
    /// `approve`, because a withdrawn approval that still replays would bind a
    /// second code to a request somebody already walked away from.
    pub(crate) retired: Rc<Cell<bool>>,
}

/// The loop the hook runs: approve, then follow the requester to a decision.
pub async fn drive(rig: ApproverRig, generation: u64) {
    if rig.is_stale(generation) {
        return;
    }
    let Some(approval) = rig.begin(generation) else {
        return;
    };
    if !rig.await_approval(&approval, generation).await {
        return;
    }
    rig.await_confirmation(&approval, generation).await;
}

impl ApproverRig {
    /// Bind the rig to the signals the hook owns.
    pub fn new(pump: Pump, state: Signal<ApproverState>, generation: Signal<u64>) -> Self {
        Self {
            pump,
            approval: Rc::new(RefCell::new(None)),
            storage: Rc::new(SessionStorageKeyValueStore::new()),
            state,
            generation,
            cancelling: Rc::new(Cell::new(false)),
            retired: Rc::new(Cell::new(false)),
        }
    }

    /// The approval's tab-scoped record store.
    pub(crate) fn ceremony(&self) -> CeremonyStore<'_> {
        CeremonyStore::new(self.storage.as_ref())
    }

    /// Whether a newer approval has superseded the loop that is running.
    pub fn is_stale(&self, generation: u64) -> bool {
        *self.generation.peek() != generation
    }

    /// Patch the list's and the dialog's state.
    pub(crate) fn patch(&self, change: impl FnOnce(&mut ApproverState)) {
        let mut next = self.state.peek().clone();
        change(&mut next);
        let mut state = self.state;
        state.set(next);
    }

    /// Take a request out of the approver's list, once nothing can answer it.
    ///
    /// The store is written through the pump's event path, the same one the
    /// sidebar and the dialogs use: a component that reached around it for a
    /// `store_mut` would be a second way to write durable state, and this is the
    /// one case where the row's fate is decided by an ANSWER from the
    /// coordinator rather than by the reader closing something.
    pub(crate) fn dismiss(&self, ephemeral_id: &str) {
        self.pump
            .dispatch(ClientEvent::Shell(ShellIntent::DismissPairRequest {
                ephemeral_id: ephemeral_id.to_string(),
            }));
    }

    /// The approval this loop follows, or `None` when there is nothing to do.
    ///
    /// Generation zero is the mount: it restores a persisted approval so an
    /// approval interrupted by a reload binds the SAME code the human was
    /// already told, and never mints one.
    pub fn begin(&self, generation: u64) -> Option<PairApproval> {
        if generation > 0 {
            return self.approval.borrow().clone();
        }
        let record = self.ceremony().load_approval()?;
        if record.expires_at_ms <= now_ms() {
            self.ceremony().clear_approval();
            return None;
        }
        *self.approval.borrow_mut() = Some(record.clone());
        self.patch(|state| {
            state.phase = ApprovalPhase::Approving;
            state.busy_request_id = Some(record.ephemeral_id.clone());
        });
        Some(record)
    }

    /// Generate a code for `ephemeral_id`, persist it, and put it in flight.
    ///
    /// Refuses a request whose expiry has already passed: the coordinator would
    /// refuse the approval anyway, and a code bound to a dead row is a code a
    /// human reads out for nothing.
    pub fn approve(&self, ephemeral_id: &str, requester_label: &str, expires_at_ms: i64) {
        if self.state.peek().phase != ApprovalPhase::Idle {
            return;
        }
        let Ok(expires) = u64::try_from(expires_at_ms) else {
            self.settle(ApprovalOutcome::Expired);
            return;
        };
        if expires <= now_ms() {
            self.settle(ApprovalOutcome::Expired);
            return;
        }
        let Ok(approval) =
            PairApproval::generate(&BrowserRandomSource, ephemeral_id, requester_label, expires)
        else {
            self.patch(|state| {
                state.notice = Some((
                    NoticeTone::Error,
                    "Could not generate a verification code.".to_string(),
                ));
            });
            return;
        };
        self.ceremony().save_approval(&approval);
        self.retired.set(false);
        *self.approval.borrow_mut() = Some(approval);
        self.patch(|state| {
            state.phase = ApprovalPhase::Approving;
            state.busy_request_id = Some(ephemeral_id.to_string());
            state.notice = None;
        });
        // The read is taken into a local first: `peek` holds a guard, and `set`
        // asks the same signal for a write.
        let next_generation = self.generation.peek().saturating_add(1);
        let mut generation = self.generation;
        generation.set(next_generation);
    }

    /// `PairApprove` until the coordinator agrees, the request dies, or the
    /// attempt is refused.
    ///
    /// `true` means the code is on screen and the requester may use it.
    async fn await_approval(&self, approval: &PairApproval, generation: u64) -> bool {
        let mut attempt = 0;
        loop {
            if self.is_stale(generation) || self.retired.get() {
                return false;
            }
            // The request's own deadline is the only local clock in this phase;
            // once the code is shown the coordinator's status read owns expiry,
            // so a skewed device clock cannot hide a code still in play.
            if approval.expires_at_ms <= now_ms() {
                self.settle(ApprovalOutcome::Expired);
                return false;
            }
            let call = ApprovePair {
                request: approval.approve_request(),
            };
            match self.pump.rpc().call(&call).await {
                Ok(answer) if answer.ok => {
                    self.patch(|state| {
                        state.phase = ApprovalPhase::AwaitingConfirmation;
                        state.dialog = CodeDialogState::Awaiting;
                        state.approval = Some(approval.clone());
                        state.busy_request_id = None;
                    });
                    // The code is bound, so this approval owns the request and
                    // the list must stop offering it as answerable. The
                    // coordinator's next snapshot is idempotent about it, and
                    // filtering the row here instead would be a second answer
                    // to "why did that row disappear".
                    self.dismiss(&approval.ephemeral_id);
                    tracing::info!(target: "auth", "auth.pair_approved");
                    return true;
                }
                Ok(_) => {
                    self.settle(ApprovalOutcome::Refused);
                    return false;
                }
                Err(error) if is_transient(&error) => {
                    let delay = retry_delay_ms(attempt);
                    attempt = attempt.saturating_add(1);
                    sleep_ms(delay).await;
                }
                Err(error) => {
                    tracing::warn!(target: "auth", %error, "pair approval refused");
                    self.settle(ApprovalOutcome::Refused);
                    return false;
                }
            }
        }
    }
}
