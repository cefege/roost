//! The device key's lifecycle over an abstract vault: load, first boot with its
//! add race, bearer minting, reset. Owned by `platform::device_key`, whose
//! `WebDeviceKey` drives it over the browser vault (IndexedDB + WebCrypto) or,
//! off-browser, over a vault that refuses everything. Target-independent so the
//! sequencing is decided by native tests. Ported from `loadOrGenerateLocked`,
//! `signCoordinatorJwt` and `resetWebKey` in `apps/web/src/client/auth/web-key.ts`.

use std::cell::RefCell;

use roost_client_core::client::auth::jwt::{CoordinatorJwt, build_unsigned_jwt, public_key_b64};
use roost_protocol::fingerprint::{FINGERPRINT_INPUT_BYTES, fingerprint_hex};

use super::bearer_cache::BearerCache;

/// The DOMException name IndexedDB gives an `add` into an occupied slot.
pub const CONSTRAINT_ERROR: &str = "ConstraintError";

/// Why the slot refused an `add`, in the shape a DOMException has.
///
/// The name is carried verbatim rather than pre-classified so the one decision
/// that matters — [`CONSTRAINT_ERROR`] means another tab's first boot won the
/// race — is made here, where it is tested, and not in the browser adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddFailure {
    /// The DOMException `name`, e.g. `ConstraintError` or `QuotaExceededError`.
    pub error_name: String,
    /// What the platform said.
    pub message: String,
}

/// Everything the lifecycle needs from a platform, and nothing it decides.
///
/// There is no `put`: first boot writes with [`KeyVault::add_current`], whose
/// refusal of an occupied slot is what makes two tabs converge on one key.
pub trait KeyVault {
    /// The platform's handle to a key pair. The private half never leaves it.
    type Pair;
    /// The key pair in the device-key slot, if there is one.
    async fn read_current(&self) -> Result<Option<Self::Pair>, String>;
    /// A fresh Ed25519 pair whose private half is not extractable.
    async fn generate(&self) -> Result<Self::Pair, String>;
    /// Write `pair` into the EMPTY device-key slot.
    async fn add_current(&self, pair: &Self::Pair) -> Result<(), AddFailure>;
    /// Empty the device-key slot. An empty slot is not an error.
    async fn delete_current(&self) -> Result<(), String>;
    /// The pair's raw public key, as exported.
    async fn export_public_key(&self, pair: &Self::Pair) -> Result<Vec<u8>, String>;
    /// SHA-256 of `bytes`.
    async fn sha256(&self, bytes: &[u8]) -> Result<Vec<u8>, String>;
    /// An Ed25519 signature over `message` by the pair's private half.
    async fn sign(&self, pair: &Self::Pair, message: &[u8]) -> Result<Vec<u8>, String>;
    /// Whether this profile has minted a key before (`roostKeyMinted`).
    fn key_was_minted(&self) -> bool;
    /// Record that this profile has minted a key.
    fn mark_key_minted(&self);
}

/// What the coordinator knows this device by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// The raw public key, standard padded base64 (`ssh_pubkey_b64`).
    pub public_key_b64: String,
    /// Lowercase hex SHA-256 of the raw public key: the JWT `kid` and `sub`.
    pub fingerprint: String,
}

impl DeviceIdentity {
    /// The identity of an exported raw public key and its SHA-256 digest.
    ///
    /// Both widths are checked rather than trusted: a 31-byte export or a
    /// digest of the wrong algorithm would otherwise become a fingerprint no
    /// coordinator can match, which surfaces only as an unexplained 401.
    pub fn from_exports(raw_public_key: &[u8], digest: &[u8]) -> Result<Self, String> {
        let raw: &[u8; 32] = raw_public_key.try_into().map_err(|_| {
            format!(
                "the exported public key is {} bytes, not 32",
                raw_public_key.len()
            )
        })?;
        let digest: &[u8; FINGERPRINT_INPUT_BYTES] = digest.try_into().map_err(|_| {
            format!(
                "the public key digest is {} bytes, not {FINGERPRINT_INPUT_BYTES}",
                digest.len()
            )
        })?;
        Ok(Self {
            public_key_b64: public_key_b64(raw),
            fingerprint: fingerprint_hex(digest),
        })
    }
}

/// A key pair in use, and the identity derived from it once.
#[derive(Debug)]
pub struct LoadedKey<P> {
    pub pair: P,
    pub identity: DeviceIdentity,
}

/// This profile's device key, generating one on a profile that has none.
pub async fn load_or_generate<V: KeyVault>(vault: &V) -> Result<LoadedKey<V::Pair>, String> {
    let Some(existing) = vault.read_current().await? else {
        return first_boot(vault).await;
    };
    let loaded = describe(vault, existing).await?;
    tracing::info!(target: "auth", fingerprint = %loaded.identity.fingerprint, "auth.device_key_loaded");
    Ok(loaded)
}

/// A bearer for one request: the cached token inside its window, else a fresh
/// signature over a token issued at `now_ms`, which then becomes the cached one.
pub async fn mint_bearer<V: KeyVault>(
    vault: &V,
    key: &LoadedKey<V::Pair>,
    cache: &RefCell<BearerCache>,
    now_ms: u64,
) -> Result<String, String> {
    // Copied out so no borrow is held across the signing await: a second
    // request on the same tab may consult the cache while this one signs.
    let reusable = cache.borrow().reusable(now_ms).map(str::to_owned);
    if let Some(token) = reusable {
        return Ok(token);
    }
    let unsigned = build_unsigned_jwt(&key.identity.fingerprint, now_ms);
    let signature = vault
        .sign(&key.pair, unsigned.signing_input.as_bytes())
        .await?;
    let jwt = CoordinatorJwt::mint(&unsigned, &signature, now_ms);
    let token = jwt.token().to_owned();
    cache.borrow_mut().store(jwt, now_ms);
    tracing::debug!(target: "auth", kid = %key.identity.fingerprint, issued_at_ms = now_ms, "auth.jwt_minted");
    Ok(token)
}

/// Empty the device-key slot, so the next load mints a new identity.
pub async fn reset<V: KeyVault>(vault: &V) -> Result<(), String> {
    vault.delete_current().await?;
    tracing::info!(target: "auth", "auth.device_key_reset");
    Ok(())
}

/// Mint, `add`, and adopt the winner when another tab's `add` got there first.
///
/// A lost race re-reads rather than retrying or overwriting: anything else
/// leaves one tab signing with a key the slot no longer holds.
async fn first_boot<V: KeyVault>(vault: &V) -> Result<LoadedKey<V::Pair>, String> {
    if vault.key_was_minted() {
        tracing::warn!(target: "auth", "auth.key_evicted");
    } else {
        tracing::info!(target: "auth", "auth.key_first_boot");
    }
    let generated = vault.generate().await?;
    match vault.add_current(&generated).await {
        Ok(()) => {
            vault.mark_key_minted();
            let loaded = describe(vault, generated).await?;
            tracing::info!(target: "auth", fingerprint = %loaded.identity.fingerprint, "auth.device_key_generated");
            Ok(loaded)
        }
        Err(failure) if failure.error_name == CONSTRAINT_ERROR => {
            let winner = vault.read_current().await?.ok_or_else(|| {
                "the device key slot refused the add but holds no key".to_owned()
            })?;
            let loaded = describe(vault, winner).await?;
            tracing::info!(target: "auth", fingerprint = %loaded.identity.fingerprint, "auth.key_first_boot_race_lost");
            Ok(loaded)
        }
        Err(failure) => Err(format!("{}: {}", failure.error_name, failure.message)),
    }
}

async fn describe<V: KeyVault>(vault: &V, pair: V::Pair) -> Result<LoadedKey<V::Pair>, String> {
    let raw = vault.export_public_key(&pair).await?;
    let digest = vault.sha256(&raw).await?;
    let identity = DeviceIdentity::from_exports(&raw, &digest)?;
    Ok(LoadedKey { pair, identity })
}

#[cfg(test)]
mod tests;
