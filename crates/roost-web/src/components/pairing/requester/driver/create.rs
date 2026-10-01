//! The requester's create: minting or restoring the ceremony, and the retries
//! around one `PairCreate`.
//!
//! A child of the driver rather than a sibling because it reads the driver's
//! private ceremony cell and its `restored` flag, and a field loosened to `pub`
//! for a sibling would be a field every sibling could reach. Split out because
//! creation and polling change for different reasons, and a driver holding
//! both would be one file over the cap. Ports the `create()` path of
//! `apps/web/src/components/pairing/onboarding-pairing-ceremony.ts:120-240`.

use roost_client_core::client::auth::PairPollStatus;
use roost_client_core::client::rpc::calls::pairing::CreatePair;
use roost_client_core::client::rpc::{CallError, ConnectCode};

use crate::components::pairing::failure::{describe, is_transient, retry_delay_ms};
use crate::components::terminal::dom::sleep_ms;
use crate::platform::self_label::current_browser_self_label;

use super::{CreateOutcome, RequesterRig, device_public_key};

impl RequesterRig {
    /// Restore this tab's ceremony, or mint one, and put it in flight.
    ///
    /// Generation one is the mount: it restores and never mints, because a
    /// browser nobody asked must not put a row on an approver's screen by having
    /// somebody read the page.
    pub async fn begin(&self, generation: u64) -> CreateOutcome {
        let session = if generation <= 1 {
            match self.restored_session() {
                Some(session) => session,
                None => return CreateOutcome::Idle,
            }
        } else {
            // An entropy failure is a refusal the reader must see, not a
            // silence: leaving the request button disabled with no sentence is
            // the one outcome a page cannot recover from on its own.
            match self.fresh_session() {
                Ok(session) => session,
                Err(message) => return CreateOutcome::Fatal(message),
            }
        };
        *self.session.borrow_mut() = Some(session);
        self.ensure_created(generation).await
    }

    /// Ask the coordinator to create the request, retrying a transient failure
    /// with the ceremony intact.
    async fn ensure_created(&self, generation: u64) -> CreateOutcome {
        let mut attempt = 0;
        loop {
            if self.is_stale(generation) {
                return CreateOutcome::Idle;
            }
            match self.send_create().await {
                CreateOutcome::Acknowledged => return CreateOutcome::Acknowledged,
                CreateOutcome::Transient => {
                    let delay = retry_delay_ms(attempt);
                    attempt = attempt.saturating_add(1);
                    self.patch(|state| state.busy = false);
                    sleep_ms(delay).await;
                }
                outcome => return outcome,
            }
        }
    }

    /// One `PairCreate`.
    async fn send_create(&self) -> CreateOutcome {
        let public_key = match device_public_key(&self.pump).await {
            Ok(public_key) => public_key,
            Err(reason) => {
                return CreateOutcome::Fatal(format!(
                    "Could not prepare browser pairing: {reason}"
                ));
            }
        };
        let request = {
            let borrowed = self.session.borrow();
            let Some(session) = borrowed.as_ref() else {
                return CreateOutcome::Idle;
            };
            session.create_request(&public_key, &current_browser_self_label())
        };
        self.patch(|state| {
            state.busy = true;
            state.failure = None;
            state.request_failure = None;
        });
        match self.pump.rpc().call_public(&CreatePair { request }).await {
            Ok(response) => {
                let mut borrowed = self.session.borrow_mut();
                let Some(session) = borrowed.as_mut() else {
                    return CreateOutcome::Idle;
                };
                match session.on_create_response(&response) {
                    Ok(()) => {
                        self.ceremony().save_ceremony(session.ceremony());
                        self.patch(|state| {
                            state.status = PairPollStatus::Pending;
                            state.busy = false;
                        });
                        CreateOutcome::Acknowledged
                    }
                    Err(_) => CreateOutcome::Fatal(
                        "Pairing request did not match this browser.".to_string(),
                    ),
                }
            }
            Err(error) => self.classify_create_failure(&error),
        }
    }

    /// Read a failed `PairCreate`.
    ///
    /// A restored ceremony whose create already committed reads as a
    /// `FailedPrecondition`, because the coordinator is refusing to create a row
    /// that exists — which is the answer the requester needed, not a failure to
    /// report (`onboarding-pairing-ceremony.ts:221-226`).
    fn classify_create_failure(&self, error: &CallError) -> CreateOutcome {
        if is_transient(error) {
            return CreateOutcome::Transient;
        }
        if self.restored.get() && error.code() == Some(&ConnectCode::FailedPrecondition) {
            if let Some(session) = self.session.borrow_mut().as_mut() {
                session.mark_acknowledged();
            }
            self.patch(|state| {
                state.status = PairPollStatus::Pending;
                state.busy = false;
            });
            return CreateOutcome::Acknowledged;
        }
        CreateOutcome::Fatal(format!("Pair create failed: {}", describe(error)))
    }
}
