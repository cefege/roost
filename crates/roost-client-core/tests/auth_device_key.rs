//! The device key's lifecycle: non-extractability, the rotation window, the
//! reset's precondition, and the two refusals that keep a weak key out.
//!
//! Every test drives the real `DeviceKeyManager` over the real
//! `MemorySecureKeyStore`, so what is under test is the ceremony rather than a
//! stand-in for it. That store is the reference implementation of the storage
//! discipline (`client/auth/memory_keystore.rs`) and deliberately not a
//! cryptography implementation; the properties asserted here are the ones a
//! browser cannot be asked about in a unit test.
//!
//! The first-boot race is in `auth_first_boot_race.rs`, because the interleaving
//! it needs is its own subject.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use base64::prelude::{BASE64_STANDARD, Engine as _};
use roost_client_core::client::auth::{
    DeviceKeyManager, KeyAdmission, KeyStoreError, MemorySecureKeyStore, ResetOutcome,
    RotationError, RotationOutcome, RotationRecovery, RotationRefusal, RotationStage,
    SecureKeyStore, recover_rotation,
};
use roost_client_core::{MemoryClock, MemoryKeyValueStore};
// `get` is a trait method on `KeyValueStore`, not inherent on the in-memory
// store; the minted-once flag assertion below needs it in scope.
use roost_client_core::KeyValueStore as _;
use support::auth::{RecordingRotator, ScriptedProbe, store_with_current};

#[test]
fn a_generated_key_is_non_extractable_and_nothing_the_store_hands_back_carries_its_bytes() {
    let store = MemorySecureKeyStore::new();
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let clock = MemoryClock::new();
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);

    let info = keys.load_or_generate().expect("a first boot mints a key");
    assert!(!info.extractable, "a generated key must not be extractable");

    let key = store.current_key().expect("the minted key is the current one");
    let descriptor = store.describe(key).expect("the key is describable");
    assert!(
        !descriptor.extractable,
        "the store must report the key as non-extractable, not merely be trusted"
    );

    // The private half is unreachable from outside, and everything the ceremony
    // emits renders the PUBLIC half. Searching each rendering for the private
    // bytes is what turns "the trait has no exporter" into a failing assertion.
    let private = store.private_bytes_for_test(key);
    assert_ne!(
        descriptor.public_key.to_vec(),
        private.to_vec(),
        "the two halves must differ, or this test proves nothing"
    );
    let private_hex: String = private.iter().map(|byte| format!("{byte:02x}")).collect();
    for (name, rendered) in [
        ("the fingerprint", descriptor.fingerprint.clone()),
        ("the public key", keys.public_key_b64().expect("public key")),
        ("the handle's debug form", format!("{key:?}")),
        (
            "the credential",
            keys.sign_coordinator_jwt().expect("a token").token().to_string(),
        ),
    ] {
        assert!(
            !rendered.contains(private_hex.as_str()),
            "{name} must not carry the private key"
        );
    }
}

#[test]
fn an_extractable_key_is_refused_rather_than_used() {
    let store = MemorySecureKeyStore::new();
    let weak = store.plant_extractable_key();
    store.force_overwrite_current(weak);
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let clock = MemoryClock::new();
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);

    assert_eq!(
        keys.load_or_generate(),
        Err(KeyStoreError::ExtractableKeyRefused {
            slot: roost_client_core::client::auth::DEVICE_KEY_SLOT
        }),
        "an extractable key is the absence of the guarantee, not a weaker one"
    );
    assert!(
        keys.current_bearer().is_none(),
        "a refused key must not produce a credential either"
    );
}

#[test]
fn a_store_without_durable_storage_fails_loudly_instead_of_minting_a_throwaway_key() {
    let store = MemorySecureKeyStore::without_persistence();
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let clock = MemoryClock::new();
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);

    assert!(matches!(
        keys.load_or_generate(),
        Err(KeyStoreError::PersistenceUnavailable { .. })
    ));
    assert!(
        store.current_key().is_none(),
        "a key that cannot outlive a reload must never be written as if it could"
    );
    assert!(
        flags
            .get(roost_client_core::client::auth::KEY_MINTED_FLAG)
            .is_none(),
        "and it must not be recorded as minted"
    );
}

#[test]
fn rotation_admits_the_old_key_for_exactly_its_window_and_the_new_one_afterwards() {
    // Before the coordinator is asked, the replacement means nothing to it and
    // the device is still the old key: discard the stage, keep the device.
    assert_eq!(
        recover_rotation(KeyAdmission::DeviceRejected, Some(KeyAdmission::Authorized)),
        RotationRecovery::Discarded
    );
    // In the window after the coordinator enrolled the replacement, both keys
    // are good, and the staged one is the one that becomes the device.
    assert_eq!(
        recover_rotation(KeyAdmission::Authorized, None),
        RotationRecovery::Promoted
    );
    assert_eq!(
        recover_rotation(
            KeyAdmission::Authorized,
            Some(KeyAdmission::Authorized)
        ),
        RotationRecovery::Promoted
    );
    // NOT the other way round. Every unanswered question is ambiguous, in both
    // directions: an unreachable coordinator is not a rejection, and a stage
    // nobody could vouch for is not an authorization.
    for unanswered in [None, Some(KeyAdmission::Ambiguous)] {
        assert_eq!(
            recover_rotation(KeyAdmission::DeviceRejected, unanswered),
            RotationRecovery::Ambiguous,
            "a rejected stage must never be discarded on an unanswered question"
        );
    }
    assert_eq!(
        recover_rotation(KeyAdmission::Ambiguous, Some(KeyAdmission::Authorized)),
        RotationRecovery::Ambiguous,
        "an unvouched stage must never be promoted, however good the old key looks"
    );
    assert_eq!(
        recover_rotation(
            KeyAdmission::DeviceRejected,
            Some(KeyAdmission::DeviceRejected)
        ),
        RotationRecovery::Ambiguous,
        "both keys rejected means there is no device left to keep"
    );
}

#[test]
fn a_completed_rotation_promotes_the_new_key_and_the_old_one_stops_signing() {
    let store = MemorySecureKeyStore::new();
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let clock = MemoryClock::new();
    let mut keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    let before = keys.load_or_generate().expect("load").fingerprint;
    let old_key = store.current_key().expect("current");
    let old_credential = keys.current_bearer().expect("a credential");

    let mut rotator = RecordingRotator::accepting("rotated-device");
    assert_eq!(
        keys.rotate_current(&mut rotator, "laptop").expect("accepted"),
        RotationOutcome::Rotated
    );
    let asked = rotator.requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].label, "laptop");
    assert_eq!(
        asked[0].bearer.as_deref(),
        Some(old_credential.as_str()),
        "the rotation is authorized by the key it is replacing"
    );
    let new_key = store.current_key().expect("promoted");
    assert_ne!(new_key, old_key);
    assert_eq!(
        BASE64_STANDARD
            .decode(&asked[0].public_key_b64)
            .expect("the request carries a standard-base64 public key"),
        store
            .describe(new_key)
            .expect("describe")
            .public_key
            .to_vec(),
        "the coordinator was asked to enroll exactly the key that was promoted"
    );
    assert_eq!(
        store.rotation_stage(),
        None,
        "promotion clears the stage in the same transaction"
    );
    let after = keys.sign_coordinator_jwt().expect("token");
    assert_ne!(
        after.kid(),
        before,
        "a rotation that keeps signing as the retired key is a lockout"
    );
    assert_eq!(after.kid(), store.describe(new_key).expect("d").fingerprint);
}

#[test]
fn a_rotation_the_coordinator_refuses_leaves_the_device_exactly_as_it_was() {
    let store = MemorySecureKeyStore::new();
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let clock = MemoryClock::new();
    let mut keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    let before = keys.load_or_generate().expect("load").fingerprint;
    let kept = store.current_key().expect("current");

    let mut rotator = RecordingRotator::refusing(RotationRefusal {
        message: "permission denied".to_string(),
        authoritative: true,
    });
    // Pinned, not `is_err()`: a key-store failure would satisfy a bare
    // `is_err()` while testing none of what this test is about.
    assert!(matches!(
        keys.rotate_current(&mut rotator, "laptop"),
        Err(RotationError::Refused(RotationRefusal { .. }))
    ));
    assert_eq!(
        store.current_key(),
        Some(kept),
        "a refused rotation must not install anything"
    );
    assert_eq!(
        keys.sign_coordinator_jwt().expect("token").kid(),
        before,
        "and the device keeps signing as the key the coordinator still accepts"
    );
    assert!(
        store.rotation_stage().is_some(),
        "the stage survives, so the next boot can tell an enrolled key from a refused one"
    );
}

#[test]
fn an_interrupted_rotation_is_resolved_by_probing_and_never_by_guessing() {
    let flags = MemoryKeyValueStore::new();
    let clock = MemoryClock::new();

    // The replacement was never enrolled and the old key still works: discard
    // the stage, keep the device.
    let store = MemorySecureKeyStore::new();
    let keep = store_with_current(&store);
    store
        .add_rotation_stage(&RotationStage {
            operation_id: "rotate-abandoned".to_string(),
            key: store.generate().expect("generate"),
        })
        .expect("stage");
    let probe = ScriptedProbe::new(
        [KeyAdmission::DeviceRejected, KeyAdmission::Authorized],
        KeyAdmission::Ambiguous,
    );
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    let info = keys.load_or_generate().expect("load");
    assert_eq!(store.current_key(), Some(keep), "the device kept its key");
    assert_eq!(store.rotation_stage(), None, "the stage was discarded");
    assert_eq!(info.fingerprint, store.describe(keep).expect("d").fingerprint);
    assert_eq!(probe.remaining(), 0, "both questions were asked");

    // The coordinator enrolled the replacement before the tab died: promote it,
    // and the old key's state is never consulted.
    let store = MemorySecureKeyStore::new();
    let _previous = store_with_current(&store);
    let enrolled = store.generate().expect("generate");
    store
        .add_rotation_stage(&RotationStage {
            operation_id: "rotate-enrolled".to_string(),
            key: enrolled,
        })
        .expect("stage");
    let probe = ScriptedProbe::new([KeyAdmission::Authorized], KeyAdmission::Ambiguous);
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    keys.load_or_generate().expect("load");
    assert_eq!(store.current_key(), Some(enrolled));
    assert_eq!(store.rotation_stage(), None);
    assert_eq!(probe.remaining(), 0, "decided without a second round trip");
}

#[test]
fn a_rotation_the_coordinator_could_not_answer_is_refused_rather_than_resolved() {
    let store = MemorySecureKeyStore::new();
    let keep = store_with_current(&store);
    store
        .add_rotation_stage(&RotationStage {
            operation_id: "rotate-unknown".to_string(),
            key: store.generate().expect("generate"),
        })
        .expect("stage");

    // The staged key cannot be vouched for, so the ceremony stops rather than
    // promoting or discarding on a question nobody answered.
    let probe = ScriptedProbe::new(
        [KeyAdmission::Ambiguous, KeyAdmission::Authorized],
        KeyAdmission::Ambiguous,
    );
    let flags = MemoryKeyValueStore::new();
    let clock = MemoryClock::new();
    let mut keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    let mut rotator = RecordingRotator::accepting("rotated-device");
    // The refusal is the unanswered question, not the coordinator saying no.
    assert!(matches!(
        keys.rotate_current(&mut rotator, "laptop"),
        Err(RotationError::Key(KeyStoreError::ProbeAmbiguous))
    ));
    assert_eq!(store.current_key(), Some(keep), "the device is untouched");
    assert!(
        store.rotation_stage().is_some(),
        "the stage is left for a later boot"
    );
    assert!(
        rotator.requests().is_empty(),
        "an unresolvable state must not reach the coordinator"
    );
}

#[test]
fn a_reset_needs_the_coordinators_explicit_rejection() {
    let clock = MemoryClock::new();
    let flags = MemoryKeyValueStore::new();

    // Still accepted: refused.
    let store = MemorySecureKeyStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let mut keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    keys.load_or_generate().expect("load");
    assert!(!keys.is_reset_eligible().expect("eligible"));
    assert_eq!(
        keys.reset(),
        Err(KeyStoreError::ResetRefused),
        "a key the coordinator still accepts must not be deleted"
    );
    assert!(store.current_key().is_some());

    // Unreachable: also refused, because silence is not consent.
    let store = MemorySecureKeyStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Ambiguous);
    let mut keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    keys.load_or_generate().expect("load");
    assert_eq!(keys.reset(), Err(KeyStoreError::ProbeAmbiguous));
    assert!(store.current_key().is_some());

    // Rejected, and only then.
    let store = MemorySecureKeyStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::DeviceRejected);
    let mut keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    keys.load_or_generate().expect("load");
    assert!(keys.is_reset_eligible().expect("eligible"));
    assert_eq!(keys.reset(), Ok(ResetOutcome::Unpaired));
    assert!(store.current_key().is_none());
    assert_eq!(
        keys.reset(),
        Ok(ResetOutcome::NotPaired),
        "a second reset has nothing to remove and must not fail"
    );
}
