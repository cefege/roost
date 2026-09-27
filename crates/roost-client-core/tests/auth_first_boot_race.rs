//! The first-boot race, the `add` that refuses an occupied slot, and the rule
//! that a signing failure never stops a request.
//!
//! Two browsers opening the pairing flow at once both find an empty key slot,
//! both generate, and both write. The write is an `add` and not a `put`, so one
//! of them is refused and re-reads what the other installed; the alternative is
//! two `put`s, which is one device silently replaced by another with its public
//! key already enrolled at the coordinator. Nothing about that failure is
//! visible on screen, which is why the race has its own file rather than a
//! paragraph in the happy path.

mod support;

use roost_client_core::client::auth::{
    AuthRedeemBrowserRequest, DeviceKeyManager, KeyAdmission, KeyStoreError, MemorySecureKeyStore,
    RedeemCall, RedeemOutcome, RedeemRefusal, RefusalCode, SecureKeyStore, redeem_pair_token,
};
use roost_client_core::{MemoryClock, MemoryKeyValueStore};
// `get` is a trait method on `KeyValueStore`, not inherent on the in-memory
// store; the minted-once flag assertion below needs it in scope.
use roost_client_core::KeyValueStore as _;
use support::auth::{LostRaceStore, ScriptedProbe, store_with_current};

/// A store with a current key already in it, for a manager that must not mint.
fn filled() -> (
    MemorySecureKeyStore,
    roost_client_core::client::auth::DeviceKey,
) {
    let store = MemorySecureKeyStore::new();
    let key = store_with_current(&store);
    (store, key)
}

#[test]
fn the_current_key_slot_is_written_with_add_and_a_second_add_is_refused() {
    let store = MemorySecureKeyStore::new();
    let first = store.generate().expect("generate");
    let second = store.generate().expect("generate");

    store
        .add_current(first)
        .expect("the first add fills the slot");
    assert_eq!(
        store.add_current(second),
        Err(KeyStoreError::AlreadyPresent),
        "a second add must refuse rather than overwrite"
    );
    assert_eq!(
        store.add_current(first),
        Err(KeyStoreError::AlreadyPresent),
        "re-adding the key already there is still a refusal, not a silent success"
    );
    assert_eq!(
        store.current_key(),
        Some(first),
        "the slot still holds the first writer's key"
    );
    assert_eq!(
        store.rotation_stage(),
        None,
        "the rotation stage is a different slot and must stay empty"
    );
    assert!(
        store.delete_current().expect("delete"),
        "the revoke path removes it"
    );
    assert!(
        !store.delete_current().expect("delete"),
        "and reports it was gone"
    );
}

#[test]
fn two_concurrent_first_boots_converge_on_exactly_one_key() {
    let store = MemorySecureKeyStore::new();

    // The interleaving two tabs produce, driven through the trait: both read
    // empty, both generate, the first add wins and the second is refused.
    assert!(store.read_current().expect("read").is_none());
    let tab_a = store.generate().expect("tab A generates");
    let tab_b = store.generate().expect("tab B generates");
    store.add_current(tab_a).expect("tab A wins the slot");
    assert_eq!(store.add_current(tab_b), Err(KeyStoreError::AlreadyPresent));

    // Both tabs now load. Neither mints a third key, and both end on the same
    // identity, so one browser cannot end up replacing the other's device.
    let flags = MemoryKeyValueStore::new();
    let clock = MemoryClock::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let first = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let second = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    assert_eq!(
        first.load_or_generate().expect("load").fingerprint,
        second.load_or_generate().expect("load").fingerprint
    );
    assert_eq!(
        first.public_key_b64().expect("public key"),
        second.public_key_b64().expect("public key"),
        "two tabs on one profile present the SAME key to the coordinator"
    );
    assert_eq!(
        store.generated_count(),
        2,
        "exactly the two raced keys were minted, and no third"
    );
    // The flag belongs to the MINTING path, and this fixture filled the slot
    // itself — v2 marks it only once its own `addCurrentWebKey` resolves
    // (`web-key.ts:176-177`), never on the adopt-an-existing arm. So the
    // assertion needs a manager that really did a first boot.
    let virgin = MemorySecureKeyStore::new();
    let virgin_flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    DeviceKeyManager::new(&virgin, &virgin_flags, &probe, &clock)
        .load_or_generate()
        .expect("first boot");
    assert_eq!(
        virgin_flags.get(roost_client_core::client::auth::KEY_MINTED_FLAG),
        Some("1".to_string()),
        "the profile records that a key was minted, so a later eviction is visible"
    );
}

#[test]
fn a_first_boot_that_loses_the_race_adopts_the_winners_key() {
    let seed = MemorySecureKeyStore::new();
    let winner = store_with_current(&seed);
    let store = LostRaceStore::new(winner);
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let clock = MemoryClock::new();
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);

    let info = keys
        .load_or_generate()
        .expect("the loser adopts the winner");
    assert_eq!(
        info.fingerprint,
        store.describe(winner).expect("describe").fingerprint,
        "the manager must sign as the key that is actually stored, not the one it minted"
    );
    assert_eq!(store.current_key(), Some(winner));
    assert_eq!(
        store.generated_count(),
        1,
        "the loser's own key is minted and then discarded, never written"
    );
    assert_eq!(
        keys.sign_coordinator_jwt().expect("token").kid(),
        info.fingerprint,
        "the credential names the adopted key"
    );
}

#[test]
fn one_manager_asked_twice_signs_as_one_key() {
    // The in-process half of the same race: a browser racing itself must not
    // mint a second key either, and a second call must not re-sign.
    let (store, minted) = filled();
    let clock = MemoryClock::new();
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    let first = keys.sign_coordinator_jwt().expect("token");
    let second = keys.sign_coordinator_jwt().expect("token");
    assert_eq!(first.token(), second.token(), "inside the reuse window");
    assert_eq!(store.generated_count(), 1, "no second key was minted");
    assert_eq!(store.current_key(), Some(minted));
}

#[test]
fn a_credential_is_reused_inside_its_window_and_re_signed_in_place_after_it() {
    let (store, _) = filled();
    let clock = MemoryClock::new();
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);

    let first = keys.sign_coordinator_jwt().expect("token");
    // The reuse window IS the cache TTL: v2 reuses while `age < JWT_CACHE_TTL_MS`
    // (`web-key.ts:219`), and the token's own lifetime is a separate, longer
    // number. Mixing the two is not a stricter test, it is a different one.
    clock.advance(roost_client_core::client::auth::JWT_CACHE_TTL_MS - 1);
    assert_eq!(
        keys.sign_coordinator_jwt().expect("token").token(),
        first.token(),
        "one millisecond of window is still inside the window"
    );
    clock.advance(1);
    let refreshed = keys.sign_coordinator_jwt().expect("token");
    assert_ne!(refreshed.token(), first.token(), "the window closed");
    assert_eq!(refreshed.kid(), first.kid(), "in place: the same key");
    assert_eq!(
        refreshed.issued_at_ms() - first.issued_at_ms(),
        roost_client_core::client::auth::JWT_CACHE_TTL_MS,
        "a refresh moves the issuance by exactly the reuse window"
    );
    assert!(
        !refreshed.is_expired(refreshed.issued_at_ms()),
        "a freshly minted credential is never born expired"
    );
}

/// Records the request a redemption made, so "the request still goes out" is
/// observable rather than asserted.
#[derive(Default)]
struct RecordingRedeem {
    seen: Vec<(AuthRedeemBrowserRequest, Option<String>)>,
    refusal: Option<RedeemRefusal>,
}

impl RedeemCall for RecordingRedeem {
    fn auth_redeem_browser(
        &mut self,
        request: &AuthRedeemBrowserRequest,
        bearer: Option<String>,
    ) -> Result<(), RedeemRefusal> {
        self.seen.push((request.clone(), bearer));
        match &self.refusal {
            Some(refusal) => Err(refusal.clone()),
            None => Ok(()),
        }
    }
}

#[test]
fn a_signing_failure_still_dispatches_the_request_unauthenticated() {
    // The store holds the key and refuses only to SIGN: the ceremony is intact
    // and the credential is not, which is the exact shape of the failure.
    let store = MemorySecureKeyStore::with_failing_signing();
    let flags = MemoryKeyValueStore::new();
    let probe = ScriptedProbe::always(KeyAdmission::Authorized);
    let clock = MemoryClock::new();
    let keys = DeviceKeyManager::new(&store, &flags, &probe, &clock);
    let public_key_b64 = keys.public_key_b64().expect("the key itself is fine");
    assert!(
        keys.current_bearer().is_none(),
        "a signing failure yields no credential"
    );

    let mut call = RecordingRedeem::default();
    let outcome = redeem_pair_token(
        &mut call,
        "roost_bt_token",
        &public_key_b64,
        "this browser",
        keys.current_bearer(),
    );
    assert_eq!(call.seen.len(), 1, "the request was still issued");
    assert_eq!(call.seen[0].1, None, "and carried no credential");
    assert_eq!(call.seen[0].0.ssh_pubkey_b64, public_key_b64);
    assert_eq!(call.seen[0].0.token, "roost_bt_token");
    assert_eq!(outcome, RedeemOutcome::Redeemed);
}

#[test]
fn a_refusal_the_coordinator_actually_decided_is_final_and_a_blip_is_not() {
    let mut decided = RecordingRedeem {
        refusal: Some(RedeemRefusal {
            message: "token already spent".to_string(),
            code: RefusalCode::AlreadyExists,
        }),
        ..RecordingRedeem::default()
    };
    let outcome = redeem_pair_token(&mut decided, "t", "k", "l", None);
    assert!(
        outcome.is_final(),
        "a token the coordinator judged is not worth retrying"
    );
    assert_eq!(
        decided.seen.len(),
        1,
        "and the request still went out, so the judgement was the coordinator's"
    );

    let mut blip = RecordingRedeem {
        refusal: Some(RedeemRefusal {
            message: "network changed".to_string(),
            code: RefusalCode::Other,
        }),
        ..RecordingRedeem::default()
    };
    let outcome = redeem_pair_token(&mut blip, "t", "k", "l", None);
    assert!(
        !outcome.is_final(),
        "a transport failure says nothing about the token"
    );
    assert_eq!(blip.seen.len(), 1, "but it was still attempted");
}
