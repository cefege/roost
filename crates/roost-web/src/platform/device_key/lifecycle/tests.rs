//! Native tests for the device-key lifecycle, over a fake vault whose futures
//! are always ready. Ports `apps/web/tests/webKeyJwtCache.test.ts` (the JWT
//! reuse window) and the first-boot add race of `web-key.ts`.

use std::cell::{Cell, RefCell};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use roost_client_core::client::auth::jwt::{build_unsigned_jwt, public_key_b64};
use roost_protocol::fingerprint::fingerprint_hex;

use super::{
    AddFailure, CONSTRAINT_ERROR, DeviceIdentity, KeyVault, LoadedKey, load_or_generate,
    mint_bearer, reset,
};
use crate::platform::device_key::bearer_cache::BearerCache;

/// 2026-07-11T00:00:00Z, the instant v2's cache test pins its clock to.
const T0_MS: u64 = 1_783_728_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakePair(u8);

#[derive(Debug, Default)]
struct FakeVault {
    slot: Cell<Option<FakePair>>,
    generated: Cell<u8>,
    signatures: Cell<u32>,
    minted_flag: Cell<bool>,
    /// A key another tab `add`s between this tab's read and its own `add`.
    interloper: Cell<Option<FakePair>>,
    /// Refuse the `add` as occupied while leaving the slot empty.
    phantom_conflict: Cell<bool>,
    add_failure: RefCell<Option<String>>,
    sign_failure: RefCell<Option<String>>,
    export_width: Cell<Option<usize>>,
}

impl KeyVault for FakeVault {
    type Pair = FakePair;

    async fn read_current(&self) -> Result<Option<FakePair>, String> {
        Ok(self.slot.get())
    }

    async fn generate(&self) -> Result<FakePair, String> {
        self.generated.set(self.generated.get() + 1);
        Ok(FakePair(self.generated.get()))
    }

    async fn add_current(&self, pair: &FakePair) -> Result<(), AddFailure> {
        if let Some(error_name) = self.add_failure.borrow().clone() {
            return Err(AddFailure {
                error_name,
                message: "the add was refused".to_owned(),
            });
        }
        if let Some(winner) = self.interloper.take() {
            self.slot.set(Some(winner));
        }
        if self.slot.get().is_some() || self.phantom_conflict.get() {
            return Err(AddFailure {
                error_name: CONSTRAINT_ERROR.to_owned(),
                message: "Key already exists in the object store.".to_owned(),
            });
        }
        self.slot.set(Some(*pair));
        Ok(())
    }

    async fn delete_current(&self) -> Result<(), String> {
        self.slot.set(None);
        Ok(())
    }

    async fn export_public_key(&self, pair: &FakePair) -> Result<Vec<u8>, String> {
        Ok(vec![pair.0; self.export_width.get().unwrap_or(32)])
    }

    async fn sha256(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        Ok(vec![bytes.first().copied().unwrap_or(0) ^ 0xa5; 32])
    }

    async fn sign(&self, pair: &FakePair, _message: &[u8]) -> Result<Vec<u8>, String> {
        if let Some(reason) = self.sign_failure.borrow().clone() {
            return Err(reason);
        }
        self.signatures.set(self.signatures.get() + 1);
        Ok(vec![pair.0; 64])
    }

    fn key_was_minted(&self) -> bool {
        self.minted_flag.get()
    }

    fn mark_key_minted(&self) {
        self.minted_flag.set(true);
    }
}

/// Drive a future whose every await is already ready.
fn ready<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(output) => output,
        Poll::Pending => panic!("the fake vault never pends"),
    }
}

fn fingerprint_of(pair: FakePair) -> String {
    fingerprint_hex(&[pair.0 ^ 0xa5; 32])
}

fn loaded(vault: &FakeVault) -> LoadedKey<FakePair> {
    match ready(load_or_generate(vault)) {
        Ok(loaded) => loaded,
        Err(reason) => panic!("load_or_generate failed: {reason}"),
    }
}

fn bearer(
    vault: &FakeVault,
    key: &LoadedKey<FakePair>,
    cache: &RefCell<BearerCache>,
    now_ms: u64,
) -> String {
    match ready(mint_bearer(vault, key, cache, now_ms)) {
        Ok(token) => token,
        Err(reason) => panic!("mint_bearer failed: {reason}"),
    }
}

#[test]
fn a_first_boot_generates_adds_and_marks_the_profile_minted() {
    let vault = FakeVault::default();
    let first = loaded(&vault);
    assert_eq!(first.pair, FakePair(1));
    assert_eq!(vault.slot.get(), Some(FakePair(1)));
    assert!(vault.minted_flag.get());
    assert_eq!(first.identity.fingerprint, fingerprint_of(FakePair(1)));
    assert_eq!(first.identity.public_key_b64, public_key_b64(&[1; 32]));

    let second = loaded(&vault);
    assert_eq!(
        second.pair,
        FakePair(1),
        "a stored key is loaded, not replaced"
    );
    assert_eq!(vault.generated.get(), 1);
}

#[test]
fn a_first_boot_that_loses_the_add_race_adopts_the_winner() {
    let vault = FakeVault::default();
    vault.interloper.set(Some(FakePair(200)));
    let adopted = loaded(&vault);
    assert_eq!(adopted.pair, FakePair(200));
    assert_eq!(adopted.identity.fingerprint, fingerprint_of(FakePair(200)));
    assert_eq!(
        vault.slot.get(),
        Some(FakePair(200)),
        "the winner is never overwritten"
    );
    assert_eq!(vault.generated.get(), 1, "the loser does not mint again");
    assert!(
        !vault.minted_flag.get(),
        "only the tab whose add landed marks the profile"
    );
}

#[test]
fn a_refused_add_with_an_empty_slot_is_an_error_not_a_second_mint() {
    let vault = FakeVault::default();
    vault.phantom_conflict.set(true);
    assert!(ready(load_or_generate(&vault)).is_err());
    assert_eq!(vault.generated.get(), 1);
}

#[test]
fn an_add_that_fails_for_another_reason_adopts_nothing() {
    let vault = FakeVault::default();
    *vault.add_failure.borrow_mut() = Some("QuotaExceededError".to_owned());
    match ready(load_or_generate(&vault)) {
        Err(reason) => assert!(reason.starts_with("QuotaExceededError"), "{reason}"),
        Ok(loaded) => panic!("adopted {:?} after a failed add", loaded.pair),
    }
    assert_eq!(vault.slot.get(), None);
    assert!(!vault.minted_flag.get());
}

#[test]
fn a_public_key_of_the_wrong_width_is_refused() {
    let vault = FakeVault::default();
    vault.export_width.set(Some(31));
    assert!(ready(load_or_generate(&vault)).is_err());
    assert!(DeviceIdentity::from_exports(&[0; 32], &[0; 20]).is_err());
}

/// v2 `webKeyJwtCache.test.ts` "reuses one token within 240s, re-mints past
/// the TTL": zero additional signatures inside the window, a new `iat` past it,
/// and the new token becomes the cached one.
#[test]
fn a_bearer_is_reused_within_the_window_and_re_minted_past_it() {
    let vault = FakeVault::default();
    let key = loaded(&vault);
    let cache = RefCell::new(BearerCache::default());

    let first = bearer(&vault, &key, &cache, T0_MS);
    assert_eq!(first.split('.').count(), 3);
    assert!(first.starts_with(&build_unsigned_jwt(&key.identity.fingerprint, T0_MS).signing_input));
    assert_eq!(vault.signatures.get(), 1);

    assert_eq!(bearer(&vault, &key, &cache, T0_MS), first);
    assert_eq!(bearer(&vault, &key, &cache, T0_MS + 239_000), first);
    assert_eq!(vault.signatures.get(), 1);

    let second = bearer(&vault, &key, &cache, T0_MS + 241_000);
    assert_ne!(second, first);
    assert!(second.starts_with(
        &build_unsigned_jwt(&key.identity.fingerprint, T0_MS + 241_000).signing_input
    ));
    assert_eq!(vault.signatures.get(), 2);

    assert_eq!(bearer(&vault, &key, &cache, T0_MS + 242_000), second);
    assert_eq!(vault.signatures.get(), 2);
}

#[test]
fn a_failed_signature_caches_nothing() {
    let vault = FakeVault::default();
    let key = loaded(&vault);
    let cache = RefCell::new(BearerCache::default());
    *vault.sign_failure.borrow_mut() = Some("OperationError".to_owned());
    assert_eq!(
        ready(mint_bearer(&vault, &key, &cache, T0_MS)),
        Err("OperationError".to_owned())
    );
    *vault.sign_failure.borrow_mut() = None;
    bearer(&vault, &key, &cache, T0_MS);
    assert_eq!(vault.signatures.get(), 1);
}

#[test]
fn a_reset_empties_the_slot_so_the_next_load_mints_a_new_identity() {
    let vault = FakeVault::default();
    let before = loaded(&vault);
    assert_eq!(ready(reset(&vault)), Ok(()));
    assert_eq!(vault.slot.get(), None);
    let after = loaded(&vault);
    assert_ne!(after.identity.fingerprint, before.identity.fingerprint);
    assert_eq!(vault.slot.get(), Some(after.pair));
}
