//! Credential records and the async persistence boundary used by the account pool.
//! The coordinator supplies a database-backed implementation; tests and standalone
//! callers can use `InMemoryCredentialStore`. Secrets remain in the credential kind.

use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};

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
    fn list<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Vec<StoredCredential>>;
    fn upsert<'a>(
        &'a self,
        provider: &'a str,
        kind: CredentialKind,
        identity_key: &'a str,
        label: &'a str,
    ) -> BoxFuture<'a, i64>;
    fn update_kind<'a>(&'a self, id: i64, kind: CredentialKind) -> BoxFuture<'a, ()>;
    fn disable<'a>(&'a self, id: i64, cause: &'a str) -> BoxFuture<'a, ()>;
    fn clear_sticky<'a>(&'a self, conversation_id: &'a str, provider: &'a str)
    -> BoxFuture<'a, ()>;
    fn delete<'a>(&'a self, id: i64) -> BoxFuture<'a, ()>;
    fn blocks<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Vec<(i64, i64)>>;
    fn set_block<'a>(&'a self, id: i64, until_ms: i64) -> BoxFuture<'a, ()>;
    fn sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, Option<i64>>;
    fn set_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
        id: i64,
    ) -> BoxFuture<'a, ()>;
}
