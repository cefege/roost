//! Authenticated encryption and row decoding for coordinator credentials.
//! The associated-data binding prevents ciphertext moving between identities.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use serde::{Deserialize, Serialize};
use sqlx::{Row as _, any::AnyRow};

use super::{CredentialKind, CredentialStoreError, StoredCredential, storage_error};

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedCredential {
    v: u8,
    nonce: String,
    ct: String,
}

pub(super) fn encode_data(
    key: Option<&[u8; 32]>,
    provider: &str,
    identity_key: &str,
    kind: &CredentialKind,
) -> Result<String, CredentialStoreError> {
    let plaintext = serde_json::to_vec(kind)
        .map_err(|error| CredentialStoreError::Serialization(error.to_string()))?;
    let Some(key) = key else {
        return String::from_utf8(plaintext)
            .map_err(|error| CredentialStoreError::Serialization(error.to_string()));
    };
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| CredentialStoreError::Encryption(error.to_string()))?;
    let mut nonce = [0_u8; 24];
    getrandom::fill(&mut nonce)
        .map_err(|error| CredentialStoreError::Encryption(error.to_string()))?;
    let aad = credential_aad(provider, identity_key);
    let ciphertext = cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|error| CredentialStoreError::Encryption(error.to_string()))?;
    serde_json::to_string(&EncryptedCredential {
        v: 1,
        nonce: STANDARD.encode(nonce),
        ct: STANDARD.encode(ciphertext),
    })
    .map_err(|error| CredentialStoreError::Serialization(error.to_string()))
}

pub(super) fn decode_credential(
    key: Option<&[u8; 32]>,
    row: &AnyRow,
) -> Result<(StoredCredential, bool), CredentialStoreError> {
    let data_json: String = row.try_get("data_json").map_err(storage_error)?;
    let provider: String = row.try_get("provider").map_err(storage_error)?;
    let identity_key: String = row.try_get("identity_key").map_err(storage_error)?;
    let (kind, needs_reencrypt) = decode_data(key, &provider, &identity_key, &data_json)?;
    Ok((
        StoredCredential {
            id: row.try_get("id").map_err(storage_error)?,
            provider,
            kind,
            identity_key,
            label: row.try_get("label").map_err(storage_error)?,
            disabled_cause: row.try_get("disabled_cause").map_err(storage_error)?,
        },
        needs_reencrypt,
    ))
}

fn decode_data(
    key: Option<&[u8; 32]>,
    provider: &str,
    identity_key: &str,
    data_json: &str,
) -> Result<(CredentialKind, bool), CredentialStoreError> {
    let value: serde_json::Value = serde_json::from_str(data_json)
        .map_err(|error| CredentialStoreError::Serialization(error.to_string()))?;
    if value.get("v").is_none() {
        let kind = serde_json::from_value(value)
            .map_err(|error| CredentialStoreError::Serialization(error.to_string()))?;
        return Ok((kind, key.is_some()));
    }
    let envelope: EncryptedCredential = serde_json::from_value(value)
        .map_err(|error| CredentialStoreError::MalformedEncryptedData(error.to_string()))?;
    if envelope.v != 1 {
        return Err(CredentialStoreError::MalformedEncryptedData(format!(
            "unsupported version {}",
            envelope.v
        )));
    }
    let key = key.ok_or(CredentialStoreError::EncryptionKeyMissing)?;
    let nonce = STANDARD
        .decode(envelope.nonce)
        .map_err(|error| CredentialStoreError::MalformedEncryptedData(error.to_string()))?;
    let nonce: [u8; 24] = nonce.try_into().map_err(|nonce: Vec<u8>| {
        CredentialStoreError::MalformedEncryptedData(format!(
            "nonce must contain 24 bytes, got {}",
            nonce.len()
        ))
    })?;
    let ciphertext = STANDARD
        .decode(envelope.ct)
        .map_err(|error| CredentialStoreError::MalformedEncryptedData(error.to_string()))?;
    let cipher = XChaCha20Poly1305::new_from_slice(key)
        .map_err(|error| CredentialStoreError::Encryption(error.to_string()))?;
    let aad = credential_aad(provider, identity_key);
    let plaintext = cipher
        .decrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| CredentialStoreError::WrongEncryptionKey)?;
    let kind = serde_json::from_slice(&plaintext)
        .map_err(|error| CredentialStoreError::Serialization(error.to_string()))?;
    Ok((kind, false))
}

fn credential_aad(provider: &str, identity_key: &str) -> String {
    format!("{provider}|{identity_key}")
}

pub(super) fn credential_kind_name(kind: &CredentialKind) -> &'static str {
    match kind {
        CredentialKind::OAuth { .. } => "oauth",
        CredentialKind::ApiKey { .. } => "api_key",
    }
}
