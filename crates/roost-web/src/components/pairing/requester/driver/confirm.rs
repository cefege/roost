//! The requester's confirmation: typing the approver's code, and what a lost
//! answer means.
//!
//! A child of the driver rather than a sibling because it reads the driver's
//! private ceremony handle, and a field loosened to `pub` for a sibling would be
//! a field every sibling could reach. Ports `confirm()` in
//! `apps/web/src/components/pairing/onboarding-pairing-ceremony.ts:321-374`.
use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::ReadableExt as _;

use roost_client_core::client::rpc::calls::pairing::ConfirmPair;

use crate::components::pairing::failure::{describe, is_transient, retry_delay_ms};
use crate::components::terminal::dom::sleep_ms;

use super::RequesterRig;

/// The in-flight marker for one confirmation, released however the attempt ends.
///
/// The marker is what keeps the ceremony's poll loop from racing the one call
/// that can finish the pairing, and that loop outlives every attempt: an
/// unmount, a dismissed dialog or a superseding request drops the confirmation
/// future mid-flight. A flag cleared only on the path a reply takes would leave
/// the loop standing aside for the rest of the ceremony — and the loop is the
/// only thing left that can finish a pairing whose confirmation lost its answer.
#[derive(Debug)]
pub struct ConfirmingGuard {
    flag: Rc<Cell<bool>>,
}

impl Drop for ConfirmingGuard {
    fn drop(&mut self) {
        self.flag.set(false);
    }
}

/// What a confirmation whose answer was lost still owes this ceremony.
///
/// Two facts, because they answer two questions. `may_have_committed` says the
/// coordinator may already hold a completed pairing, which is what makes a LATER
/// refusal ambiguous instead of final; `owed` says a `PairPoll` is still due to
/// find out which. The ceremony's own record names the request, so the recovery
/// is a poll and never a second confirmation — a mutation the coordinator could
/// not tell from a replay of one it already committed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmRecovery {
    may_have_committed: bool,
    owed: bool,
    attempt: u32,
}

impl ConfirmRecovery {
    /// Whether a poll is owed to a lost confirmation.
    pub fn is_owed(&self) -> bool {
        self.owed
    }

    /// Whether the coordinator may already have committed the confirmation.
    pub fn may_have_committed(&self) -> bool {
        self.may_have_committed
    }

    /// Whether an answer to a poll that was NOT asked for the lost confirmation
    /// may be believed.
    ///
    /// Such a poll was already in flight when the answer was lost, so it asked
    /// about the ceremony BEFORE the confirmation and cannot settle what the
    /// confirmation left open.
    pub fn accepts_plain_answer(&self) -> bool {
        !self.owed
    }

    /// The answer was lost after the coordinator may have committed it. Returns
    /// how long to wait for the recovery poll: the ceremony's own backoff, so
    /// recovering a confirmation is patient in exactly the way recovering a
    /// create is.
    pub fn lose_answer(&mut self) -> u64 {
        self.may_have_committed = true;
        self.owed = true;
        self.next_attempt_delay()
    }

    /// A call the coordinator refused while a lost confirmation may already have
    /// committed. Returns zero: it has just answered something, and the reader
    /// is watching for what became of the request.
    pub fn refuse_after_lost_answer(&mut self) -> u64 {
        self.owed = true;
        0
    }

    /// The owed poll answered and the ceremony is still live, so the next poll is
    /// an ordinary one.
    ///
    /// The ambiguity does NOT end here: a refusal between this answer and the
    /// ceremony's end is still a refusal of something that may already be paired,
    /// so only the poll that ends the ceremony may forget it.
    pub fn clear_owed(&mut self) {
        self.owed = false;
        self.attempt = 0;
    }

    /// The ceremony ended or was abandoned; nothing is owed and nothing is
    /// ambiguous.
    pub fn retire(&mut self) {
        self.may_have_committed = false;
        self.clear_owed();
    }

    fn next_attempt_delay(&mut self) -> u64 {
        let delay = retry_delay_ms(self.attempt);
        self.attempt = self.attempt.saturating_add(1);
        delay
    }
}

impl RequesterRig {
    /// Send `PairConfirm` for the code in the card's field, and settle.
    pub async fn confirm(&self) {
        // A second attempt while one is in flight, or while a lost answer is
        // still owed its poll, would send the ceremony's only mutation twice.
        if self.is_confirming() || self.recovery.borrow().is_owed() {
            return;
        }
        let typed = self.state.peek().verification_code.clone();
        let request = {
            let borrowed = self.session.borrow();
            let Some(session) = borrowed.as_ref() else {
                return;
            };
            match session.confirm_request(&typed) {
                Ok(request) => request,
                Err(_) => {
                    self.patch(|state| {
                        state.failure =
                            Some("Enter the six-digit code from the paired browser.".to_string());
                    });
                    return;
                }
            }
        };
        let _in_flight = self.begin_confirming();
        self.patch(|state| {
            state.busy = true;
            state.failure = None;
        });
        let outcome = self.pump.rpc().call_public(&ConfirmPair { request }).await;
        // Read once, before the arms choose: a refusal is only final while no
        // earlier answer went missing.
        let lost_answer_may_have_committed = self.recovery.borrow().may_have_committed();
        match outcome {
            Ok(response) if response.ok => {
                if let Some(session) = self.session.borrow_mut().as_mut() {
                    session.on_confirm_response(&response);
                }
                self.forget_ceremony();
                self.complete();
            }
            // A code that did not match is the coordinator's considered answer,
            // not a failure: the ceremony is still live and a second attempt is
            // exactly what the reader should be offered.
            Ok(_) => self.patch(|state| {
                state.busy = false;
                state.failure =
                    Some("That code did not match. Check it and try again.".to_string());
            }),
            // The confirmation MAY have committed and lost its answer, so the
            // only honest next step is a poll: reporting a failure here would
            // tell a browser that is already paired that it is not, and sending
            // the confirm again is the replay this ceremony cannot survive.
            Err(error) if is_transient(&error) => {
                self.patch(|state| {
                    state.failure =
                        Some("Confirmation interrupted. Checking pairing status.".to_string());
                });
                self.recover_lost_answer().await;
            }
            // A refusal that arrives after an earlier answer was lost is
            // ambiguous for the same reason: the coordinator may be refusing the
            // ceremony it already completed, so a status is still the only answer
            // allowed to settle it.
            Err(_) if lost_answer_may_have_committed => {
                self.patch(|state| {
                    state.failure = Some("Checking pairing status.".to_string());
                });
                self.recover_refused_confirmation().await;
            }
            // Refused for a request the coordinator never took on: there is
            // nothing to poll for, and the ceremony has to be asked for again.
            Err(error) => self.fail(format!("Confirmation failed: {}", describe(&error))),
        }
    }

    /// Mark a confirmation in flight until the returned guard is dropped.
    fn begin_confirming(&self) -> ConfirmingGuard {
        self.confirming.set(true);
        ConfirmingGuard {
            flag: Rc::clone(&self.confirming),
        }
    }

    /// Ask the coordinator what became of a confirmation whose answer was lost,
    /// after the backoff the ceremony already uses for a lost create.
    async fn recover_lost_answer(&self) {
        let delay = self.recovery.borrow_mut().lose_answer();
        tracing::warn!(
            target: "auth",
            delay_ms = delay,
            "pair confirmation interrupted; recovering by poll"
        );
        sleep_ms(delay).await;
        self.poll_once().await;
    }

    /// The same recovery for a refusal that arrived after a lost answer, asked at
    /// once because the coordinator has just answered something.
    async fn recover_refused_confirmation(&self) {
        let delay = self.recovery.borrow_mut().refuse_after_lost_answer();
        tracing::warn!(
            target: "auth",
            "pair confirmation refused after a lost answer; recovering by poll"
        );
        sleep_ms(delay).await;
        self.poll_once().await;
    }
}

#[cfg(test)]
mod tests {
    use roost_client_core::client::auth::{
        PAIR_REQUEST_ID_BYTES, PAIR_REQUESTER_TOKEN_BYTES, PAIRING_CEREMONY_VERSION,
        PairConfirmRequest, PairCreateResponse, PairPollResponse, PairPollStatus, PairingCeremony,
        PairingSession,
    };

    use super::*;

    /// A ceremony whose values are in their canonical spelling, which is what a
    /// tab-scoped record read back always looks like.
    fn ceremony() -> PairingCeremony {
        PairingCeremony {
            ceremony_version: PAIRING_CEREMONY_VERSION,
            ephemeral_id: "a".repeat(PAIR_REQUEST_ID_BYTES * 2),
            requester_token: "b".repeat(PAIR_REQUESTER_TOKEN_BYTES * 2),
        }
    }

    fn poll_answer(status: &str) -> PairPollResponse {
        PairPollResponse {
            status: status.to_string(),
            expires_at_ms: 0,
        }
    }

    /// A ceremony the coordinator has created and an approver has bound a code
    /// to: the only stage a confirmation may be sent from.
    fn awaiting_verification() -> PairingSession {
        let values = ceremony();
        let mut session = match PairingSession::restore(values.clone()) {
            Some(session) => session,
            None => panic!("canonical ceremony values must restore"),
        };
        assert!(
            session
                .on_create_response(&PairCreateResponse {
                    ephemeral_id: values.ephemeral_id.clone(),
                })
                .is_ok(),
            "the echoed request id is the only evidence the request exists"
        );
        let status = session.on_poll_response(&poll_answer("verification_required"));
        assert_eq!(status, PairPollStatus::VerificationRequired);
        session
    }

    /// The confirmation a browser sends for a ceremony that has a code bound.
    fn confirm_request(session: &PairingSession) -> PairConfirmRequest {
        match session.confirm_request("123456") {
            Ok(request) => request,
            Err(_) => panic!("a bound code must be confirmable"),
        }
    }

    #[test]
    fn a_confirmation_whose_answer_is_lost_owes_a_poll_for_the_same_request() {
        let session = awaiting_verification();
        let sent = confirm_request(&session);

        let mut recovery = ConfirmRecovery::default();
        assert!(!recovery.may_have_committed());
        assert!(recovery.lose_answer() > 0);
        assert!(recovery.is_owed());
        assert!(recovery.may_have_committed());

        // The recovery asks about the very request whose confirmation went
        // missing: the ceremony's own record is what names it, so there is
        // nothing to re-derive and no second mutation to send.
        let owed = match session.poll_request() {
            Some(request) => request,
            None => panic!("a ceremony awaiting verification is still pollable"),
        };
        assert_eq!(owed.ephemeral_id, sent.ephemeral_id);
        assert_eq!(owed.requester_token, sent.requester_token);
        assert_eq!(owed.ceremony_version, sent.ceremony_version);
    }

    #[test]
    fn a_ceremony_whose_confirmation_was_lost_completes_on_the_polled_status() {
        let mut session = awaiting_verification();
        let _sent = confirm_request(&session);
        let mut recovery = ConfirmRecovery::default();
        recovery.lose_answer();

        let status = session.on_poll_response(&poll_answer("completed"));
        assert_eq!(status, PairPollStatus::Completed);
        assert!(status.is_terminal());
        // The completion arrived through a poll; a finished ceremony is not asked
        // about again, which is the shape of "settled by the server's status".
        assert!(session.poll_request().is_none());
    }

    #[test]
    fn a_refusal_before_the_coordinator_had_the_request_owes_no_poll() {
        let recovery = ConfirmRecovery::default();
        assert!(!recovery.may_have_committed());
        assert!(!recovery.is_owed());

        // A ceremony the coordinator never acknowledged has no poll to make, so
        // there is nothing for a refusal to recover: the reader asks again.
        let created = match PairingSession::restore(ceremony()) {
            Some(session) => session,
            None => panic!("canonical ceremony values must restore"),
        };
        assert!(created.poll_request().is_none());
    }

    #[test]
    fn a_poll_in_flight_when_the_answer_was_lost_cannot_settle_the_ceremony() {
        let mut recovery = ConfirmRecovery::default();
        assert!(recovery.accepts_plain_answer());
        recovery.lose_answer();
        assert!(!recovery.accepts_plain_answer());

        // Once the owed poll has answered, later polls are ordinary ones again —
        // but the ceremony is still ambiguous, so a refusal in between is a
        // recovery rather than the end.
        recovery.clear_owed();
        assert!(recovery.accepts_plain_answer());
        assert!(recovery.may_have_committed());
        assert_eq!(recovery.refuse_after_lost_answer(), 0);
        assert!(recovery.is_owed());

        // Only the ceremony's own end forgets the ambiguity, and it forgets the
        // debt with it.
        recovery.retire();
        assert!(!recovery.may_have_committed());
        assert!(!recovery.is_owed());
    }

    #[test]
    fn a_confirmation_dropped_mid_flight_releases_the_marker_the_poll_loop_watches() {
        let flag = Rc::new(Cell::new(false));
        let guard = ConfirmingGuard {
            flag: Rc::clone(&flag),
        };
        flag.set(true);
        assert!(flag.get(), "a live confirmation stands the poll loop aside");

        // No reply ever arrives: the future is dropped by the unmount, the closed
        // dialog or the superseded request, and the marker must go with it.
        drop(guard);
        assert!(
            !flag.get(),
            "an abandoned confirmation must not stand the poll loop aside forever"
        );
    }

    #[test]
    fn a_repeated_lost_answer_waits_longer_than_the_first() {
        let mut recovery = ConfirmRecovery::default();
        let first = recovery.lose_answer();
        let second = recovery.lose_answer();
        assert!(first > 0 && second > first, "backoff {first} then {second}");
    }
}
