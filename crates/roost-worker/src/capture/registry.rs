//! The armed recorders of this worker process, plus the one-shot ledgers an
//! unarmed manual capture needs for idempotency. Ports `apps/worker/src/diag/
//! terminal-capture-registry.ts`; held behind `super::recorder`'s lock. Lease
//! expiry is decided on SERVER time and disarms the recorder the moment it is
//! observed: an expired lease is never silently renewed.

use std::collections::HashMap;

use roost_protocol::terminal_capture::TERMINAL_CAPTURE_LIMITS;

use super::now_ms;
use super::recorder_state::{CaptureLedger, WorkerRecorder};

/// One armed recording and the ledger that lives and dies with it. Two fields
/// rather than one so a capture can hold the recorder and its ledger at once.
#[derive(Debug)]
pub struct ArmedRecording {
    pub recorder: WorkerRecorder,
    pub ledger: CaptureLedger,
}

/// Ledgers for captures taken with no lease, oldest first and bounded by
/// `max_recordings_per_process`.
#[derive(Debug, Default)]
pub struct OneShotLedgers(Vec<(String, CaptureLedger)>);

impl OneShotLedgers {
    /// The session's ledger, created (evicting the oldest past the bound) when
    /// it has none.
    pub fn ledger(&mut self, session_id: &str) -> &mut CaptureLedger {
        let position = match self.0.iter().position(|(held, _)| held == session_id) {
            Some(position) => position,
            None => {
                while self.0.len() >= TERMINAL_CAPTURE_LIMITS.max_recordings_per_process {
                    self.0.remove(0);
                }
                self.0
                    .push((session_id.to_owned(), CaptureLedger::default()));
                self.0.len() - 1
            }
        };
        &mut self.0[position].1
    }

    pub fn ledger_if_present(&self, session_id: &str) -> Option<&CaptureLedger> {
        self.0
            .iter()
            .find(|(held, _)| held == session_id)
            .map(|(_, ledger)| ledger)
    }

    pub fn ledger_if_present_mut(&mut self, session_id: &str) -> Option<&mut CaptureLedger> {
        self.0
            .iter_mut()
            .find(|(held, _)| held == session_id)
            .map(|(_, ledger)| ledger)
    }

    fn forget(&mut self, session_id: &str) {
        self.0.retain(|(held, _)| held != session_id);
    }
}

/// Every armed recorder and every one-shot ledger, keyed by session id.
#[derive(Debug, Default)]
pub struct Registry {
    pub recorders: HashMap<String, ArmedRecording>,
    pub one_shot: OneShotLedgers,
}

impl Registry {
    pub fn armed_count(&self) -> usize {
        self.recorders.len()
    }

    /// The live recording for `session_id`, disarming it first when its lease
    /// has passed.
    pub fn active(&mut self, session_id: &str) -> Option<&mut ArmedRecording> {
        if !self.disarm_if_expired(session_id) {
            return None;
        }
        self.recorders.get_mut(session_id)
    }

    /// Whether `session_id` still holds a live lease; an expired one is
    /// disarmed HERE, on observation.
    pub fn disarm_if_expired(&mut self, session_id: &str) -> bool {
        let Some(expires_at_ms) = self
            .recorders
            .get(session_id)
            .map(|armed| armed.recorder.expires_at_ms)
        else {
            return false;
        };
        if now_ms() < expires_at_ms {
            return true;
        }
        if let Some(armed) = self.recorders.remove(session_id) {
            tracing::info!(
                session_id,
                recording_id = %armed.recorder.recording_id,
                expires_at_ms,
                "terminal.capture_expired: a recording lease passed and was disarmed"
            );
        }
        false
    }

    pub fn register(&mut self, recorder: WorkerRecorder) {
        let session_id = recorder.session_id.clone();
        self.recorders.insert(
            session_id,
            ArmedRecording {
                recorder,
                ledger: CaptureLedger::default(),
            },
        );
    }

    pub fn forget_recorder(&mut self, session_id: &str) -> Option<ArmedRecording> {
        self.recorders.remove(session_id)
    }

    /// Session close or channel teardown: every record and frozen frame this
    /// session owned goes. Whether a recording was armed.
    pub fn drop_session(&mut self, session_id: &str) -> bool {
        self.one_shot.forget(session_id);
        self.recorders.remove(session_id).is_some()
    }
}
