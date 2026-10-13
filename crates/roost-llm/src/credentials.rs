//! Credential records and the async persistence boundary used by the account pool.
//! The coordinator supplies a database-backed implementation; tests and standalone
//! callers can use `InMemoryCredentialStore`. Secrets remain in the credential kind.

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;

pub type RefreshOperation<'a> = Box<
    dyn FnOnce(StoredCredential) -> BoxFuture<'a, Result<CredentialKind, LlmError>> + Send + 'a,
>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CredentialStoreError {
    #[error("credential storage failed: {0}")]
    Storage(String),
    #[error("credential data could not be serialized: {0}")]
    Serialization(String),
    #[error("encrypted credential requires ROOST_COORDINATOR_SECRET_KEY")]
    EncryptionKeyMissing,
    #[error("credential could not be decrypted with the configured key")]
    WrongEncryptionKey,
    #[error("credential encryption failed: {0}")]
    Encryption(String),
    #[error("encrypted credential data is malformed: {0}")]
    MalformedEncryptedData(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredCredential {
    pub id: i64,
    pub provider: String,
    pub kind: CredentialKind,
    pub identity_key: String,
    pub label: String,
    pub disabled_cause: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CredentialKind {
    OAuth {
        access: String,
        refresh: String,
        expires_ms: i64,
        account_id: Option<String>,
        email: Option<String>,
    },
    ApiKey {
        key: String,
    },
}

pub trait CredentialStore: Send + Sync {
    fn list<'a>(
        &'a self,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Vec<StoredCredential>, CredentialStoreError>>;
    fn upsert<'a>(
        &'a self,
        provider: &'a str,
        kind: CredentialKind,
        identity_key: &'a str,
        label: &'a str,
    ) -> BoxFuture<'a, Result<i64, CredentialStoreError>>;
    fn update_kind<'a>(
        &'a self,
        id: i64,
        kind: CredentialKind,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>>;
    fn disable<'a>(
        &'a self,
        id: i64,
        cause: &'a str,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>>;
    fn clear_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>>;
    fn delete<'a>(&'a self, id: i64) -> BoxFuture<'a, Result<(), CredentialStoreError>>;
    fn blocks<'a>(
        &'a self,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(i64, i64)>, CredentialStoreError>>;
    fn set_block<'a>(
        &'a self,
        id: i64,
        until_ms: i64,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>>;
    fn sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Option<i64>, CredentialStoreError>>;
    fn set_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
        id: i64,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>>;
    fn refresh_locked<'a>(
        &'a self,
        id: i64,
        refresh: RefreshOperation<'a>,
    ) -> BoxFuture<'a, Result<StoredCredential, LlmError>>;
}
