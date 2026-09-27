//! Shared doubles for the auth ceremony tests: a probe that answers in the
//! order it is asked, a rotator that records, a store that loses the first-boot
//! race on demand, and the two small decoders the credential assertions need.
//!
//! One copy of each, because a second copy of a probe is a second definition of
//! what the coordinator said — which is the one value in these tests a reader
//! must be able to trust. Every double drives the REAL `MemorySecureKeyStore`;
//! none of them reimplements a storage rule.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use base64::prelude::{BASE64_URL_SAFE_NO_PAD, Engine as _};
use roost_client_core::client::auth::{
    DeviceKey, DeviceKeyProbe, DeviceKeyRotator, KeyAdmission, KeyDescriptor, KeyStoreError,
    MemorySecureKeyStore, RotationRefusal, RotationRequest, RotationStage, SecureKeyStore,
};

/// A probe that answers from a queue, in the order it is asked.
///
/// A queue rather than a map keyed by fingerprint because the ORDER is the
/// contract: a staged key is always asked about before the current one, and a
/// map would let a test pass with those two reversed.
pub struct ScriptedProbe {
    answers: RefCell<VecDeque<KeyAdmission>>,
    default: KeyAdmission,
    asked: RefCell<Vec<String>>,
}

impl ScriptedProbe {
    /// A probe that answers `answers` in order, then `default`.
    pub fn new(
        answers: impl IntoIterator<Item = KeyAdmission>,
        default: KeyAdmission,
    ) -> Self {
        Self {
            answers: RefCell::new(answers.into_iter().collect()),
            default,
            asked: RefCell::new(Vec::new()),
        }
    }

    /// A probe with one standing answer and no queue.
    pub fn always(admission: KeyAdmission) -> Self {
        Self::new(Vec::new(), admission)
    }

    /// How many scripted answers are still unasked.
    pub fn remaining(&self) -> usize {
        self.answers.borrow().len()
    }

    /// Every credential this probe was shown, in order.
    pub fn asked(&self) -> Vec<String> {
        self.asked.borrow().clone()
    }
}

impl DeviceKeyProbe for ScriptedProbe {
    fn probe_bearer(&self, bearer: &str) -> KeyAdmission {
        self.asked.borrow_mut().push(bearer.to_string());
        self.answers
            .borrow_mut()
            .pop_front()
            .unwrap_or(self.default)
    }
}

/// A rotator that records what it was asked and answers what it is told to.
#[derive(Default)]
pub struct RecordingRotator {
    requests: RefCell<Vec<RotationRequest>>,
    refusal: Option<RotationRefusal>,
    fingerprint: String,
}

impl RecordingRotator {
    /// A rotator that accepts and reports this as the new device's fingerprint.
    pub fn accepting(fingerprint: &str) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            refusal: None,
            fingerprint: fingerprint.to_string(),
        }
    }

    /// A rotator the coordinator refuses; it never reports a fingerprint.
    pub fn refusing(refusal: RotationRefusal) -> Self {
        Self {
            requests: RefCell::new(Vec::new()),
            refusal: Some(refusal),
            fingerprint: String::new(),
        }
    }

    /// Every request this rotator was asked to perform.
    pub fn requests(&self) -> Vec<RotationRequest> {
        self.requests.borrow().clone()
    }
}

impl DeviceKeyRotator for RecordingRotator {
    fn rotate_current(&mut self, request: &RotationRequest) -> Result<String, RotationRefusal> {
        self.requests.borrow_mut().push(request.clone());
        match &self.refusal {
            Some(refusal) => Err(refusal.clone()),
            None => Ok(self.fingerprint.clone()),
        }
    }
}

/// A store whose first `add_current` loses to a key planted first.
///
/// The interleaving two tabs produce cannot be produced by calling the manager
/// twice, because the second call finds the slot already filled and never
/// reaches the race. This reproduces the exact order — read empty, generate,
/// add — so the manager's loser path runs for real.
pub struct LostRaceStore {
    inner: MemorySecureKeyStore,
    winner: Option<DeviceKey>,
    armed: Cell<bool>,
}

impl LostRaceStore {
    /// A store whose first `add_current` is refused, with `winner` already in
    /// the slot at that moment.
    pub fn new(winner: DeviceKey) -> Self {
        Self {
            inner: MemorySecureKeyStore::new(),
            winner: Some(winner),
            armed: Cell::new(true),
        }
    }
}

impl SecureKeyStore for LostRaceStore {
    fn generate(&self) -> Result<DeviceKey, KeyStoreError> {
        self.inner.generate()
    }

    fn describe(&self, key: DeviceKey) -> Result<KeyDescriptor, KeyStoreError> {
        self.inner.describe(key)
    }

    fn read_current(&self) -> Result<Option<DeviceKey>, KeyStoreError> {
        self.inner.read_current()
    }

    fn add_current(&self, key: DeviceKey) -> Result<(), KeyStoreError> {
        if self.armed.replace(false) {
            if let Some(winner) = self.winner {
                self.inner.force_overwrite_current(winner);
            }
            return Err(KeyStoreError::AlreadyPresent);
        }
        self.inner.add_current(key)
    }

    fn delete_current(&self) -> Result<bool, KeyStoreError> {
        self.inner.delete_current()
    }

    fn sign(&self, key: DeviceKey, message: &[u8]) -> Result<Vec<u8>, KeyStoreError> {
        self.inner.sign(key, message)
    }

    fn read_rotation_stage(&self) -> Result<Option<RotationStage>, KeyStoreError> {
        self.inner.read_rotation_stage()
    }

    fn add_rotation_stage(&self, stage: &RotationStage) -> Result<(), KeyStoreError> {
        self.inner.add_rotation_stage(stage)
    }

    fn delete_rotation_stage(&self, operation_id: &str) -> Result<bool, KeyStoreError> {
        self.inner.delete_rotation_stage(operation_id)
    }

    fn promote_rotation_stage(&self, stage: &RotationStage) -> Result<(), KeyStoreError> {
        self.inner.promote_rotation_stage(stage)
    }
}

/// The `sub` a credential names, read back out of the token.
///
/// Parsed the way the coordinator parses it — three dot-separated segments, the
/// payload base64url-decoded — so a test that depends on `sub` also fails if the
/// header or the segment order ever changes.
pub fn credential_subject(token: &str) -> String {
    let payload = token.split('.').nth(1).unwrap_or_default().to_string();
    let bytes = BASE64_URL_SAFE_NO_PAD.decode(payload).unwrap_or_default();
    let text = String::from_utf8(bytes).unwrap_or_default();
    text.split("\"sub\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string()
}

/// The claim `name` from a credential's payload, or an empty string.
pub fn credential_claim(token: &str, name: &str) -> String {
    let payload = token.split('.').nth(1).unwrap_or_default().to_string();
    let bytes = BASE64_URL_SAFE_NO_PAD.decode(payload).unwrap_or_default();
    let text = String::from_utf8(bytes).unwrap_or_default();
    let needle = format!("\"{name}\":\"");
    text.split(&needle)
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_default()
        .to_string()
}

/// The unsigned header of a credential, decoded.
pub fn credential_header(token: &str) -> String {
    let header = token.split('.').next().unwrap_or_default().to_string();
    let bytes = BASE64_URL_SAFE_NO_PAD.decode(header).unwrap_or_default();
    String::from_utf8(bytes).unwrap_or_default()
}

/// A store with `current` already filled, for a test that is not about the race.
pub fn store_with_current(store: &MemorySecureKeyStore) -> DeviceKey {
    let key = store.generate().expect("generate");
    store.add_current(key).expect("fill the slot");
    key
}
