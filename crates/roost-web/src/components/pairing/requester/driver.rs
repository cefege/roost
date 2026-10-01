//! The requester's driver: the timers, the retries and the evidence handling
//! around one `PairingSession`.
//!
//! Owned by the `PairingRequester` hook that renders the request card.
//! `PairingSession` owns the ceremony's stages; this owns everything that is
//! not a stage — when to call, what to do with a failure, and when to stop.
//! Split out because the two halves change for different reasons, and a driver
//! that also rendered the card would be one file over the cap.
//!
//! Ports the operation object and its three actions in
//! `apps/web/src/components/pairing/onboarding-pairing-ceremony.ts:33-47,120-374`.

mod confirm;
mod create;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::client::auth::{CeremonyStore, PairPollStatus, PairingSession};
use roost_client_core::client::rpc::calls::pairing::PollPair;

use crate::components::pairing::failure::{describe, is_transient};
use crate::platform::device_key::WebDeviceKey;
use crate::platform::secrets::BrowserRandomSource;
use crate::platform::storage::SessionStorageKeyValueStore;
use crate::pump::Pump;

use self::confirm::ConfirmRecovery;

use super::RequesterState;

/// How often a live request asks what became of it.
pub const POLL_INTERVAL_MS: u64 = 5_000;

/// Why a `PairCreate` did not settle, as the loop that owns it needs to know.
pub enum CreateOutcome {
    /// The coordinator has the request; poll it from here on.
    Acknowledged,
    /// Worth another attempt with the same ceremony.
    Transient,
    /// The ceremony cannot continue, and the sentence a reader must see.
    Fatal(String),
    /// Nothing was restored, so there is nothing to do until a request is asked
    /// for.
    Idle,
}

/// The requester's mutable half, shared between the render and the loop.
#[derive(Clone)]
pub struct RequesterRig {
    pump: Pump,
    session: Rc<RefCell<Option<PairingSession>>>,
    storage: Rc<SessionStorageKeyValueStore>,
    state: Signal<RequesterState>,
    generation: Signal<u64>,
    /// Set while a confirmation is in flight, so the poll loop stands aside
    /// rather than racing the one call that can finish the ceremony.
    confirming: Rc<Cell<bool>>,
    /// What a confirmation whose answer was lost still owes the ceremony: the
    /// coordinator may already hold the pairing, and one poll is still due to
    /// find out. Owned here because the poll that settles it outlives the
    /// confirmation that armed it.
    recovery: Rc<RefCell<ConfirmRecovery>>,

    /// Whether the live ceremony was restored from this tab's record rather
    /// than minted on this click. Only a restored ceremony may read a
    /// `FailedPrecondition` on re-create as "the coordinator already has it".
    restored: Rc<Cell<bool>>,
}

impl RequesterRig {
    /// Bind the rig to the signals the hook owns.
    pub fn new(pump: Pump, state: Signal<RequesterState>, generation: Signal<u64>) -> Self {
        Self {
            pump,
            session: Rc::new(RefCell::new(None)),
            storage: Rc::new(SessionStorageKeyValueStore::new()),
            state,
            generation,
            confirming: Rc::new(Cell::new(false)),
            recovery: Rc::new(RefCell::new(ConfirmRecovery::default())),
            restored: Rc::new(Cell::new(false)),
        }
    }

    /// The ceremony's tab-scoped record store.
    fn ceremony(&self) -> CeremonyStore<'_> {
        CeremonyStore::new(self.storage.as_ref())
    }

    /// Whether a newer request has superseded the loop that asks.
    pub fn is_stale(&self, generation: u64) -> bool {
        *self.generation.peek() != generation
    }

    /// Whether the reader has already been shown the end of this ceremony.
    pub fn is_finished(&self) -> bool {
        let state = self.state.peek();
        state.failed || state.status.is_terminal()
    }

    /// Read what a loop wrote, without subscribing that loop to its own writes.
    pub fn state(&self) -> RequesterState {
        self.state.peek().clone()
    }

    /// Whether a confirmation is in flight, so the poll loop stands aside.
    pub fn is_confirming(&self) -> bool {
        self.confirming.get()
    }

    /// Patch the card's state.
    fn patch(&self, change: impl FnOnce(&mut RequesterState)) {
        let mut next = self.state.peek().clone();
        change(&mut next);
        let mut state = self.state;
        state.set(next);
    }

    /// One `PairPoll`, and whatever its answer settles.
    ///
    /// A poll taken while a lost confirmation still owes one IS that poll, and
    /// the ceremony's own record names the request it asks about: the recovery
    /// is a read of what the coordinator already decided, never a second
    /// confirmation the coordinator could not tell from a replay.
    pub async fn poll_once(&self) {
        let request = {
            let borrowed = self.session.borrow();
            borrowed.as_ref().and_then(PairingSession::poll_request)
        };
        let Some(request) = request else {
            return;
        };
        let recovering = self.recovery.borrow().is_owed();
        let outcome = self.pump.rpc().call_public(&PollPair { request }).await;
        // A poll that was already in flight when a confirmation lost its answer
        // asked about the ceremony BEFORE the confirmation, so its answer
        // cannot settle what the confirmation left open — including by making
        // the card look as though nothing was interrupted.
        if !recovering && !self.recovery.borrow().accepts_plain_answer() {
            return;
        }
        match outcome {
            Ok(response) => {
                let status = self
                    .session
                    .borrow_mut()
                    .as_mut()
                    .map(|session| session.on_poll_response(&response));
                self.apply_poll_status(status.clone());
                // A status that ended the ceremony retired the recovery along
                // with the record it belonged to; a status that did not clears
                // the debt, because the request has now been read once more and
                // the next poll is an ordinary one.
                if recovering && !status.as_ref().is_some_and(PairPollStatus::is_terminal) {
                    self.recovery.borrow_mut().clear_owed();
                }
            }
            Err(error) if is_transient(&error) => {
                if recovering {
                    tracing::warn!(
                        target: "auth",
                        "pair poll interrupted while recovering a lost confirmation; the next tick asks again"
                    );
                }
            }
            Err(error) => self.fail(format!("Pair poll failed: {}", describe(&error))),
        }
    }

    /// Turn a poll's status into what the reader sees.
    ///
    /// Every terminal status clears this tab's record: the requester token is a
    /// capability, and a ceremony that has ended must not stay finishable from a
    /// tab that outlived it.
    fn apply_poll_status(&self, status: Option<PairPollStatus>) {
        match status {
            Some(PairPollStatus::Pending) => self.patch(|state| {
                state.status = PairPollStatus::Pending;
                state.failure = None;
                state.busy = false;
            }),
            Some(PairPollStatus::VerificationRequired) => self.patch(|state| {
                state.status = PairPollStatus::VerificationRequired;
                state.failure = None;
                state.busy = false;
            }),
            // Spelled out rather than guarded on `is_terminal()`: a guarded arm
            // is not an exhaustive one, and an unlisted status would then fall
            // through to the "unknown status" refusal below.
            Some(
                status @ (PairPollStatus::Completed
                | PairPollStatus::Denied
                | PairPollStatus::Expired
                | PairPollStatus::VerificationFailed),
            ) => {
                self.forget_ceremony();
                if status == PairPollStatus::Completed {
                    self.complete();
                } else {
                    self.patch(|state| {
                        state.status = status;
                        state.verification_code.clear();
                        state.failure = None;
                        state.busy = false;
                    });
                }
            }
            Some(PairPollStatus::Unknown(name)) => {
                self.fail(format!(
                    "The coordinator reported an unknown pairing status ({name})."
                ));
            }
            Some(PairPollStatus::Idle) | None => {}
        }
    }

    /// Abandon the ceremony and every trace of it in this tab.
    pub fn clear(&self) {
        self.advance_generation();
        self.forget_ceremony();
        *self.session.borrow_mut() = None;
        self.restored.set(false);
        self.confirming.set(false);
        let mut state = self.state;
        state.set(RequesterState::idle());
    }

    /// Start a fresh request, superseding whatever was on screen.
    pub fn start(&self) {
        *self.session.borrow_mut() = None;
        self.restored.set(false);
        self.recovery.borrow_mut().retire();
        self.confirming.set(false);
        let mut state = self.state;
        state.set(RequesterState::asking());
        self.advance_generation();
    }

    /// Move to the next generation, so the running loop retires and its
    /// replacement starts on the new ceremony.
    ///
    /// The read is taken into a local first: `peek` holds a guard, and `set`
    /// asks the same signal for a write.
    fn advance_generation(&self) {
        let next = self.generation.peek().saturating_add(1);
        let mut generation = self.generation;
        generation.set(next);
    }

    /// The ceremony this tab persisted, or `None` when there is none.
    fn restored_session(&self) -> Option<PairingSession> {
        let record = self.ceremony().load_ceremony()?;
        let session = PairingSession::restore(record)?;
        self.restored.set(true);
        self.patch(|state| {
            state.status = PairPollStatus::Pending;
            state.busy = true;
        });
        Some(session)
    }

    /// A ceremony with values minted now, or the sentence a reader needs when
    /// the host has no entropy — which a browser without Web Crypto genuinely
    /// does not.
    fn fresh_session(&self) -> Result<PairingSession, String> {
        let session = PairingSession::create(&BrowserRandomSource)
            .map_err(|error| format!("Could not start browser pairing: {error}"))?;
        self.patch(|state| {
            state.status = PairPollStatus::Pending;
            state.busy = true;
        });
        Ok(session)
    }

    /// Drop the tab-scoped record, so a finished ceremony cannot be replayed.
    ///
    /// The ambiguity a lost confirmation leaves behind dies with the record: the
    /// only question it could still answer was what became of a request this
    /// tab can no longer name.
    fn forget_ceremony(&self) {
        self.ceremony().clear_ceremony();
        self.recovery.borrow_mut().retire();
    }

    /// The ceremony is enrolled; the key is trusted and the workbench can mount.
    fn complete(&self) {
        self.patch(|state| {
            state.status = PairPollStatus::Completed;
            state.verification_code.clear();
            state.failure = None;
            state.busy = false;
        });
        tracing::info!(target: "auth", "browser verified; opening home");
        crate::platform::location::replace_location("/");
    }

    /// Retire the ceremony on a failure retrying cannot fix, and say why on the
    /// page.
    ///
    /// The stage goes back to `Idle` so the card offers its primary action
    /// again: a failed request with no way to ask for another is a dead end, and
    /// the only thing that changed for the reader is that asking again may work.
    pub fn fail(&self, message: String) {
        self.forget_ceremony();
        *self.session.borrow_mut() = None;
        self.patch(|state| {
            state.status = PairPollStatus::Idle;
            state.failed = true;
            state.verification_code.clear();
            state.failure = None;
            state.request_failure = Some(message);
            state.busy = false;
        });
    }
}

/// This browser's public key, from the pump's key or a fresh load.
///
/// The pump's copy is the one every other signed call presents, so a ceremony
/// that created a request under a DIFFERENT key would leave an approver
/// approving a browser that can never confirm.
async fn device_public_key(pump: &Pump) -> Result<String, String> {
    if let Some(key) = pump.rpc().device_key() {
        return Ok(key.public_key_b64().to_owned());
    }
    WebDeviceKey::load_or_generate()
        .await
        .map(|key| key.public_key_b64().to_owned())
}
