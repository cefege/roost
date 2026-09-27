//! The reference `SecureKeyStore`: in-memory, deterministic, and faithful to the
//! discipline the trait exists to state.
//!
//! This is the implementation every test drives, and it is the specification in
//! executable form: the current-key slot is written with an `add` that refuses
//! an occupied slot, a rotation stage is promoted only when the slot still holds
//! that operation, and the private half of every key never leaves this struct.
//! A host that disagrees with this implementation disagrees with the trait
//! document.
//!
//! **It is not a cryptography implementation, and nothing here is a reference
//! for signing.** The keys are 32 deterministic bytes, a "signature" is the
//! private key's bytes each incremented by one, and no verifier anywhere should
//! accept one. What the stand-in DOES reproduce faithfully is the part a browser
//! cannot be asked about in a unit test: that `generate` yields a key whose
//! private half is unreachable from outside, that two first boots cannot both
//! win the current-key slot, and that a promotion which lost its slot changes
//! nothing. Real Ed25519 is WebCrypto's, and `roost-web`'s platform module is
//! where it lives.

use std::cell::RefCell;
use std::collections::BTreeMap;

use roost_protocol::fingerprint::fingerprint_hex;

use crate::client::auth::keystore::{
    DeviceKey, KeyDescriptor, KeyStoreError, RotationStage, SecureKeyStore,
};

/// A private key's stand-in: 32 bytes the store never hands out.
const PRIVATE_KEY_BYTES: usize = 32;
/// The shape an ed25519 signature has, so a caller that assumed a length is not
/// surprised by the test double.
const SIGNATURE_BYTES: usize = 64;

/// The store's whole state.
#[derive(Debug, Default)]
struct KeyState {
    next_generation: u64,
    /// The private and public halves, by handle. Dropped with the store, exactly
    /// as a browser tab's memory does; the durable half is the host's business.
    material: BTreeMap<u64, KeyMaterial>,
    current: Option<DeviceKey>,
    stage: Option<RotationStage>,
    minted: u64,
}

/// One key's two halves.
#[derive(Debug, Clone)]
struct KeyMaterial {
    private: [u8; PRIVATE_KEY_BYTES],
    public: [u8; 32],
    extractable: bool,
}

/// A `SecureKeyStore` held entirely in this process.
///
/// `Default` is HAND-WRITTEN, and that is the whole point of the type. The two
/// capability flags are `bool`, so a `#[derive(Default)]` would set both to
/// `false` — which makes `new()` a store that can neither store nor sign, and
/// makes `without_persistence` and `with_failing_signing` no-ops rather than
/// the two ways of breaking a working store. Every test that wants a working
/// store would then have to opt back IN to one.
#[derive(Debug)]
pub struct MemorySecureKeyStore {
    state: RefCell<KeyState>,
    persistence_available: RefCell<bool>,
    signing_available: RefCell<bool>,
}

impl Default for MemorySecureKeyStore {
    /// A store that stores and signs; the degraded shapes are the two named
    /// constructors, each of which is reachable only by asking for it.
    fn default() -> Self {
        Self {
            state: RefCell::new(KeyState::default()),
            persistence_available: RefCell::new(true),
            signing_available: RefCell::new(true),
        }
    }
}

impl MemorySecureKeyStore {
    /// A store with nothing minted and storage available.
    pub fn new() -> Self {
        Self::default()
    }

    /// A store that reports its durable storage as unavailable.
    ///
    /// For the one test that matters most about a missing key store: the
    /// ceremony must fail LOUDLY rather than mint a throwaway key that pairs
    /// again on every reload.
    pub fn without_persistence() -> Self {
        let store = Self::default();
        *store.persistence_available.borrow_mut() = false;
        store
    }

    /// A store whose signing always fails.
    ///
    /// For the one rule that is otherwise untestable here: a device that cannot
    /// sign still has to be able to make its calls, unauthenticated. A store
    /// that fails to SIGN and not to STORE is the honest shape of that — the
    /// key is present and the ceremony is intact; only the credential is not.
    pub fn with_failing_signing() -> Self {
        let store = Self::default();
        *store.signing_available.borrow_mut() = false;
        store
    }

    /// How many keys this store has generated since it was created.
    ///
    /// A first-boot race mints two and keeps one, so this is not the assertion
    /// that matters; `current_key` identity is. It is here so a test can prove
    /// the loser's key was never written anywhere.
    pub fn generated_count(&self) -> u64 {
        self.state.borrow().minted
    }

    /// The current key, or `None`.
    pub fn current_key(&self) -> Option<DeviceKey> {
        self.state.borrow().current
    }

    /// The stage slot's contents, or `None`.
    pub fn rotation_stage(&self) -> Option<RotationStage> {
        self.state.borrow().stage.clone()
    }

    /// Plant a key whose private half is extractable.
    ///
    /// The ONLY way to produce one, and it exists so the refusal of an
    /// extractable key is a tested behaviour rather than an assertion about a
    /// host this crate cannot run. Every other generated key is
    /// non-extractable.
    pub fn plant_extractable_key(&self) -> DeviceKey {
        let mut state = self.state.borrow_mut();
        let generation = state.next_generation;
        state.next_generation += 1;
        state.minted += 1;
        let key = DeviceKey::from_generation(generation);
        state.material.insert(
            generation,
            KeyMaterial {
                private: private_bytes(generation),
                public: public_bytes(generation),
                extractable: true,
            },
        );
        key
    }

    /// Put `key` in the current slot by OVERWRITING, the way a `put` would.
    ///
    /// Present only so a test can prove that `add_current` is what the ceremony
    /// uses and that the overwrite is what the ceremony must never do. The
    /// ceremony never calls it: there is no path to it from
    /// `SecureKeyStore`.
    pub fn force_overwrite_current(&self, key: DeviceKey) {
        self.state.borrow_mut().current = Some(key);
    }

    /// The private bytes this store holds for `key`, for a test that must prove
    /// they do not appear in anything the ceremony produces.
    ///
    /// Deliberately NOT on the trait. It is a test-only door into the stand-in,
    /// and its existence is the reason the trait itself has no equivalent.
    pub fn private_bytes_for_test(&self, key: DeviceKey) -> [u8; PRIVATE_KEY_BYTES] {
        self.state
            .borrow()
            .material
            .get(&key.generation())
            .map(|material| material.private)
            .unwrap_or([0u8; PRIVATE_KEY_BYTES])
    }

    fn require_persistence(&self) -> Result<(), KeyStoreError> {
        if *self.persistence_available.borrow() {
            return Ok(());
        }
        Err(KeyStoreError::PersistenceUnavailable {
            detail: "this store keeps keys in memory only".to_string(),
        })
    }

    fn material(&self, key: DeviceKey) -> Result<KeyMaterial, KeyStoreError> {
        self.state
            .borrow()
            .material
            .get(&key.generation())
            .cloned()
            .ok_or(KeyStoreError::MissingCurrentKey)
    }
}

/// The stand-in private key for a generation.
///
/// Derived rather than random so a failing test names the key that misbehaved,
/// and so two stores built the same way hold the same bytes.
fn private_bytes(generation: u64) -> [u8; PRIVATE_KEY_BYTES] {
    let mut bytes = [0u8; PRIVATE_KEY_BYTES];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = generation.wrapping_mul(31).wrapping_add(index as u64) as u8;
    }
    bytes
}

/// A stand-in 32-byte digest of a public key.
///
/// Deterministic and distinct per stand-in key, and NOT a hash: the real digest
/// is WebCrypto's SHA-256, and the only thing under test here is that ONE
/// spelling of a fingerprint reaches the coordinator. It is rendered with
/// `roost_protocol::fingerprint::fingerprint_hex` so even the stand-in cannot
/// introduce a second hex spelling.
fn stand_in_digest(public: &[u8; 32]) -> [u8; 32] {
    let mut digest = [0u8; 32];
    for (index, slot) in digest.iter_mut().enumerate() {
        let mut accumulator: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in public {
            accumulator ^= u64::from(*byte).wrapping_add(index as u64);
            accumulator = accumulator.wrapping_mul(0x0100_0000_01b3);
        }
        *slot = (accumulator >> 24) as u8;
    }
    digest
}

/// The stand-in public key: a different function of the generation, so a test
/// that finds the private bytes in a request cannot be satisfied by a public key
/// that happens to share them.
fn public_bytes(generation: u64) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = generation.wrapping_mul(17).wrapping_add(index as u64).wrapping_add(0xa5) as u8;
    }
    bytes
}

impl SecureKeyStore for MemorySecureKeyStore {
    fn generate(&self) -> Result<DeviceKey, KeyStoreError> {
        let mut state = self.state.borrow_mut();
        let generation = state.next_generation;
        state.next_generation += 1;
        state.minted += 1;
        let key = DeviceKey::from_generation(generation);
        state.material.insert(
            generation,
            KeyMaterial {
                private: private_bytes(generation),
                public: public_bytes(generation),
                extractable: false,
            },
        );
        Ok(key)
    }

    fn read_current(&self) -> Result<Option<DeviceKey>, KeyStoreError> {
        Ok(self.state.borrow().current)
    }

    fn add_current(&self, key: DeviceKey) -> Result<(), KeyStoreError> {
        self.require_persistence()?;
        let mut state = self.state.borrow_mut();
        if state.current.is_some() {
            return Err(KeyStoreError::AlreadyPresent);
        }
        state.current = Some(key);
        Ok(())
    }

    fn delete_current(&self) -> Result<bool, KeyStoreError> {
        self.require_persistence()?;
        Ok(self.state.borrow_mut().current.take().is_some())
    }

    fn describe(&self, key: DeviceKey) -> Result<KeyDescriptor, KeyStoreError> {
        let material = self.material(key)?;
        Ok(KeyDescriptor {
            fingerprint: fingerprint_hex(&stand_in_digest(&material.public)),
            public_key: material.public,
            extractable: material.extractable,
        })
    }

    fn sign(&self, key: DeviceKey, message: &[u8]) -> Result<Vec<u8>, KeyStoreError> {
        if !*self.signing_available.borrow() {
            return Err(KeyStoreError::Signing {
                detail: "this store was built to refuse signing".to_string(),
            });
        }
        let material = self.material(key)?;
        // A stand-in signature: the private bytes, each incremented, over a
        // length-tagged message. Incrementing rather than copying is what lets
        // the test assert the private bytes are ABSENT from the output, which a
        // verbatim copy would make untestable.
        let mut signature = Vec::with_capacity(SIGNATURE_BYTES);
        for (index, byte) in material.private.iter().enumerate() {
            signature.push(byte.wrapping_add(1).wrapping_add(index as u8));
        }
        for byte in message {
            signature.push(*byte);
        }
        signature.truncate(SIGNATURE_BYTES);
        Ok(signature)
    }

    fn read_rotation_stage(&self) -> Result<Option<RotationStage>, KeyStoreError> {
        Ok(self.state.borrow().stage.clone())
    }

    fn add_rotation_stage(&self, stage: &RotationStage) -> Result<(), KeyStoreError> {
        self.require_persistence()?;
        let mut state = self.state.borrow_mut();
        if state.stage.is_some() {
            return Err(KeyStoreError::AlreadyPresent);
        }
        state.stage = Some(stage.clone());
        Ok(())
    }

    fn delete_rotation_stage(&self, operation_id: &str) -> Result<bool, KeyStoreError> {
        self.require_persistence()?;
        let mut state = self.state.borrow_mut();
        let matches = state
            .stage
            .as_ref()
            .is_some_and(|stage| stage.operation_id == operation_id);
        if matches {
            state.stage = None;
        }
        Ok(matches)
    }

    fn promote_rotation_stage(&self, stage: &RotationStage) -> Result<(), KeyStoreError> {
        self.require_persistence()?;
        let mut state = self.state.borrow_mut();
        match state.stage.as_ref() {
            Some(current) if current.operation_id == stage.operation_id => {}
            Some(_) => return Err(KeyStoreError::RotationStageChanged),
            None => return Err(KeyStoreError::MissingRotationStage),
        }
        if !state.material.contains_key(&stage.key.generation()) {
            return Err(KeyStoreError::MissingCurrentKey);
        }
        state.current = Some(stage.key);
        state.stage = None;
        Ok(())
    }
}
