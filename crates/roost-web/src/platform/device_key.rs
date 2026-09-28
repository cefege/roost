//! The browser's device identity: a non-extractable WebCrypto Ed25519 pair kept
//! in IndexedDB, and the coordinator JWT bearers it signs. Owned by `platform`;
//! called by the pump and the pairing surfaces. Depends on the client core's
//! JWT builder and `roost_protocol::fingerprint`. Ported from
//! `apps/web/src/client/auth/web-key.ts` and `web-key-storage.ts`, whose
//! database, store, slot and flag names this reads unchanged.
//!
//! The sequencing — load, first boot and its add race, the bearer reuse
//! window, reset — is `lifecycle`, target-independent and natively tested. The
//! `wasm32` build drives it over `browser_vault` (WebCrypto + `indexed_db`);
//! every other build drives it over a vault that has no key store, so every
//! async entry point answers `Err` instead of pretending to hold a key.
//!
//! Not here: v2's rotation-stage recovery and a reset's precondition (the
//! coordinator must have rejected this key) both need a coordinator probe, and
//! are the caller's to establish before [`WebDeviceKey::reset`].

use std::cell::RefCell;

mod bearer_cache;
mod lifecycle;
pub mod schema;

#[cfg(target_arch = "wasm32")]
mod browser_vault;
#[cfg(target_arch = "wasm32")]
mod indexed_db;

use bearer_cache::BearerCache;
use lifecycle::{KeyVault, LoadedKey};

#[cfg(target_arch = "wasm32")]
type PlatformVault = browser_vault::BrowserVault;
#[cfg(not(target_arch = "wasm32"))]
type PlatformVault = NoBrowserVault;

type PlatformPair = <PlatformVault as KeyVault>::Pair;

/// This build's vault. Holds no state: every operation reaches the platform.
fn platform_vault() -> PlatformVault {
    #[cfg(target_arch = "wasm32")]
    {
        browser_vault::BrowserVault
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        NoBrowserVault
    }
}

/// What every entry point answers on a build with no browser.
pub const NO_BROWSER_KEY_STORE: &str = "this build has no browser key store";

/// This browser profile's device key, loaded once and signing on demand.
///
/// One per document: the bearer cache lives here, so a second instance signs
/// its own tokens rather than sharing the first one's.
#[derive(Debug)]
pub struct WebDeviceKey {
    key: LoadedKey<PlatformPair>,
    bearer_cache: RefCell<BearerCache>,
}

impl WebDeviceKey {
    /// This profile's key, generating and persisting one on a profile that has
    /// none. A first boot that loses the IndexedDB `add` race to another tab
    /// adopts that tab's key.
    pub async fn load_or_generate() -> Result<Self, String> {
        let key = lifecycle::load_or_generate(&platform_vault()).await?;
        Ok(Self {
            key,
            bearer_cache: RefCell::new(BearerCache::default()),
        })
    }

    /// The raw public key, standard padded base64, as `ssh_pubkey_b64` wants.
    pub fn public_key_b64(&self) -> &str {
        &self.key.identity.public_key_b64
    }

    /// Lowercase hex SHA-256 of the raw public key: the JWT `kid`.
    pub fn fingerprint(&self) -> &str {
        &self.key.identity.fingerprint
    }

    /// A coordinator JWT for a request made at `now_ms`: the cached one while
    /// it is inside its reuse window, otherwise freshly signed and cached.
    pub async fn bearer(&self, now_ms: u64) -> Result<String, String> {
        lifecycle::mint_bearer(
            &platform_vault(),
            &self.key,
            &self.bearer_cache,
            now_ms,
        )
        .await
    }

    /// Delete this profile's key. The next [`Self::load_or_generate`] mints a
    /// new identity; an instance already loaded keeps signing with the old one,
    /// so the caller drops it.
    pub async fn reset() -> Result<(), String> {
        lifecycle::reset(&platform_vault()).await
    }
}

/// The key handle of a build with no browser: there is never one to hold.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
enum NoBrowserPair {}

/// The vault of a build with no browser. Every operation refuses, so the
/// lifecycle over it is total and never holds a key.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug)]
struct NoBrowserVault;

#[cfg(not(target_arch = "wasm32"))]
impl KeyVault for NoBrowserVault {
    type Pair = NoBrowserPair;

    async fn read_current(&self) -> Result<Option<NoBrowserPair>, String> {
        Err(NO_BROWSER_KEY_STORE.to_owned())
    }

    async fn generate(&self) -> Result<NoBrowserPair, String> {
        Err(NO_BROWSER_KEY_STORE.to_owned())
    }

    async fn add_current(&self, pair: &NoBrowserPair) -> Result<(), lifecycle::AddFailure> {
        match *pair {}
    }

    async fn delete_current(&self) -> Result<(), String> {
        Err(NO_BROWSER_KEY_STORE.to_owned())
    }

    async fn export_public_key(&self, pair: &NoBrowserPair) -> Result<Vec<u8>, String> {
        match *pair {}
    }

    async fn sha256(&self, _bytes: &[u8]) -> Result<Vec<u8>, String> {
        Err(NO_BROWSER_KEY_STORE.to_owned())
    }

    async fn sign(&self, pair: &NoBrowserPair, _message: &[u8]) -> Result<Vec<u8>, String> {
        match *pair {}
    }

    fn key_was_minted(&self) -> bool {
        false
    }

    fn mark_key_minted(&self) {}
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::{NO_BROWSER_KEY_STORE, WebDeviceKey};

    fn ready<F: Future>(future: F) -> F::Output {
        match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("a build with no browser never pends"),
        }
    }

    #[test]
    fn a_build_with_no_browser_refuses_to_load_or_reset_a_key() {
        match ready(WebDeviceKey::load_or_generate()) {
            Err(reason) => assert_eq!(reason, NO_BROWSER_KEY_STORE),
            Ok(key) => panic!("a native build loaded {key:?}"),
        }
        assert_eq!(ready(WebDeviceKey::reset()), Err(NO_BROWSER_KEY_STORE.to_owned()));
    }
}
