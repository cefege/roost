//! The browser vault: WebCrypto Ed25519 and SHA-256, IndexedDB persistence
//! (`indexed_db`), and the `roostKeyMinted` flag in `localStorage`. Owned by
//! `platform::device_key`, which hands it to the lifecycle as its `KeyVault`.
//! `wasm32` only; web-sys calls and nothing that decides. Ported from
//! `generateKeyPair`, `fingerprintFor` and `signCoordinatorJwtWithKeyPair` in
//! `apps/web/src/client/auth/web-key.ts`.

use js_sys::{Array, ArrayBuffer, Uint8Array};
use roost_client_core::KeyValueStore as _;
use roost_client_core::client::auth::keystore::KEY_MINTED_FLAG;
use wasm_bindgen::{JsCast as _, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{CryptoKeyPair, SubtleCrypto};

use super::indexed_db::{self, describe_js, key_pair_from};
use super::lifecycle::{AddFailure, KeyVault};
use crate::platform::storage::LocalStorageKeyValueStore;

/// The WebCrypto algorithm name for both key generation and signing.
const ED25519: &str = "Ed25519";

/// The digest the fingerprint is taken over.
const SHA_256: &str = "SHA-256";

/// The export format that yields the bare 32-byte public key.
const RAW_FORMAT: &str = "raw";

/// This document's WebCrypto, IndexedDB and `localStorage`.
#[derive(Debug)]
pub struct BrowserVault;

impl KeyVault for BrowserVault {
    type Pair = CryptoKeyPair;

    async fn read_current(&self) -> Result<Option<CryptoKeyPair>, String> {
        indexed_db::read_current().await
    }

    async fn generate(&self) -> Result<CryptoKeyPair, String> {
        let usages = Array::of2(&JsValue::from_str("sign"), &JsValue::from_str("verify"));
        // `false`: the private half can never be exported, by anyone, including
        // this code. IndexedDB stores the handle by structured clone.
        let promise = subtle()?
            .generate_key_with_str(ED25519, false, &usages)
            .map_err(|e| describe_js(&e))?;
        key_pair_from(settle(promise).await?)
    }

    async fn add_current(&self, pair: &CryptoKeyPair) -> Result<(), AddFailure> {
        indexed_db::add_current(pair).await
    }

    async fn delete_current(&self) -> Result<(), String> {
        indexed_db::delete_current().await
    }

    async fn export_public_key(&self, pair: &CryptoKeyPair) -> Result<Vec<u8>, String> {
        let promise = subtle()?
            .export_key(RAW_FORMAT, &pair.get_public_key())
            .map_err(|e| describe_js(&e))?;
        bytes_of(settle(promise).await?)
    }

    async fn sha256(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let promise = subtle()?
            .digest_with_str_and_u8_array(SHA_256, bytes)
            .map_err(|e| describe_js(&e))?;
        bytes_of(settle(promise).await?)
    }

    async fn sign(&self, pair: &CryptoKeyPair, message: &[u8]) -> Result<Vec<u8>, String> {
        let promise = subtle()?
            .sign_with_str_and_u8_array(ED25519, &pair.get_private_key(), message)
            .map_err(|e| describe_js(&e))?;
        bytes_of(settle(promise).await?)
    }

    fn key_was_minted(&self) -> bool {
        LocalStorageKeyValueStore::new()
            .get(KEY_MINTED_FLAG)
            .as_deref()
            == Some("1")
    }

    fn mark_key_minted(&self) {
        LocalStorageKeyValueStore::new().set(KEY_MINTED_FLAG, "1");
    }
}

fn subtle() -> Result<SubtleCrypto, String> {
    let window = web_sys::window().ok_or("this document has no window")?;
    Ok(window.crypto().map_err(|e| describe_js(&e))?.subtle())
}

async fn settle(promise: js_sys::Promise) -> Result<JsValue, String> {
    JsFuture::from(promise).await.map_err(|e| describe_js(&e))
}

fn bytes_of(value: JsValue) -> Result<Vec<u8>, String> {
    let buffer = value
        .dyn_into::<ArrayBuffer>()
        .map_err(|_| "WebCrypto answered with something other than bytes".to_owned())?;
    Ok(Uint8Array::new(&buffer).to_vec())
}
