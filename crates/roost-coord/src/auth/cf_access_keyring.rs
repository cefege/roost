//! The half of Cloudflare Access that reaches the network: fetching the key set,
//! caching it, and checking an RS256 signature against it.
//!
//! Split from `cf_access.rs` because the two halves fail differently. That file
//! decides what an assertion must SATISFY and can be exercised with a key ring
//! that answers from a table; this file decides how this coordinator REACHES
//! Cloudflare's keys and can only be exercised against a real one. Keeping them
//! apart is what makes the first testable and the second small enough to be
//! obvious.
//!
//! THE CACHE IS HERE BECAUSE THE KEY SET IS THE NETWORK'S. A gate that rebuilt
//! it per request would be one Cloudflare slowdown from refusing every pairing,
//! so the TTL and the unknown-`kid` refetch floor are part of this type's
//! contract rather than of the gate's.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use roost_host::b64url_decode;
use rsa::pkcs1v15::{Signature, VerifyingKey};
use rsa::sha2::Sha256;
use rsa::signature::Verifier;
use rsa::{BigUint, RsaPublicKey};
use serde_json::Value;

use crate::auth::cf_access::{CloudflareJwks, JWKS_REFETCH_MIN_INTERVAL_MS, JWKS_TTL_MS};

/// Cloudflare's key set over HTTPS, cached per issuer, and RSA-SHA256
/// verification against it.
#[derive(Debug, Default)]
pub struct RsaJwks {
    client: reqwest::Client,
    issuers: Mutex<HashMap<String, IssuerKeys>>,
}

#[derive(Debug, Default)]
struct IssuerKeys {
    document: Value,
    fetched_at_ms: i64,
    last_refetch_ms: i64,
}

#[async_trait]
impl CloudflareJwks for RsaJwks {
    async fn jwk(&self, issuer: &str, kid: &str) -> Result<Option<String>, String> {
        let now_ms = crate::rpc::service::now_ms();
        // THE CRITICAL SECTION ENDS BEFORE THE FIRST AWAIT, and the values it
        // needs are copied out rather than borrowed. A guard held across an await
        // is `!Send` as well as a deadlock against the next caller, and a
        // `MutexGuard` derived from `HashMap::entry`'s `&mut` does not own the
        // lock it reads through -- so this returns the MAP's guard, scoped, and
        // never an entry reference.
        let (cached, fetched_at_ms, last_refetch_ms) = {
            let mut issuers = self.issuer_keys();
            let keys = issuers.entry(issuer.to_owned()).or_default();
            (
                jwk_in(&keys.document, kid),
                keys.fetched_at_ms,
                keys.last_refetch_ms,
            )
        };
        if let Some(jwk) = cached {
            return Ok(Some(jwk));
        }
        // Inside the TTL a missing `kid` may refetch but not inside the floor.
        if now_ms - fetched_at_ms < JWKS_TTL_MS
            && now_ms - last_refetch_ms < JWKS_REFETCH_MIN_INTERVAL_MS
        {
            return Ok(None);
        }
        let body = self
            .client
            .get(format!("{issuer}/cdn-cgi/access/certs"))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|error| error.to_string())?
            .text()
            .await
            .map_err(|error| error.to_string())?;
        let document: Value =
            serde_json::from_str(&body).map_err(|error| format!("jwks is not json: {error}"))?;
        if document.get("keys").and_then(Value::as_array).is_none() {
            return Err("jwks has no keys array".to_owned());
        }
        let found = {
            let mut issuers = self.issuer_keys();
            let keys = issuers.entry(issuer.to_owned()).or_default();
            keys.document = document;
            keys.fetched_at_ms = now_ms;
            keys.last_refetch_ms = now_ms;
            jwk_in(&keys.document, kid)
        };
        tracing::debug!(issuer, "cf_access.jwks_fetched");
        Ok(found)
    }

    fn verify_rs256(&self, jwk: &str, signing_input: &str, signature: &[u8]) -> bool {
        let key: Value = serde_json::from_str(jwk).unwrap_or(Value::Null);
        let member = |name: &str| {
            key.get(name)
                .and_then(Value::as_str)
                .and_then(|value| b64url_decode(value).ok())
        };
        // A JWK member is unpadded URL-safe base64, so the token codec fits.
        let (Some(modulus), Some(exponent)) = (member("n"), member("e")) else {
            return false;
        };
        RsaPublicKey::new(
            BigUint::from_bytes_be(&modulus),
            BigUint::from_bytes_be(&exponent),
        )
        .is_ok_and(|public_key| {
            // A signature whose length is not the modulus size is refused by
            // `TryFrom` before any modular arithmetic runs.
            Signature::try_from(signature).is_ok_and(|signature| {
                VerifyingKey::<Sha256>::new(public_key)
                    .verify(signing_input.as_bytes(), &signature)
                    .is_ok()
            })
        })
    }
}

impl RsaJwks {
    /// The MAP's guard, scoped. Returning a guard derived from `entry`'s `&mut`
    /// would leave a `&mut IssuerKeys` alive after the lock was gone.
    fn issuer_keys(&self) -> std::sync::MutexGuard<'_, HashMap<String, IssuerKeys>> {
        self.issuers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The JWK a `kid` names; a malformed entry is skipped, not fatal.
fn jwk_in(document: &Value, kid: &str) -> Option<String> {
    let entry = document
        .get("keys")?
        .as_array()?
        .iter()
        .find(|entry| entry.get("kid").and_then(Value::as_str) == Some(kid))?;
    serde_json::to_string(entry).ok()
}
