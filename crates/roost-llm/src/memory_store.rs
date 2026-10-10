//! In-memory implementation of the account persistence boundary.
//! Used by pool tests and lightweight agent integrations; a single async mutex
//! keeps credential, block, and sticky updates atomic with respect to callers.

use std::{collections::HashMap, sync::Arc};

use futures::future::BoxFuture;
use tokio::sync::Mutex;

use crate::credentials::{CredentialKind, CredentialStore, StoredCredential};

#[derive(Debug, Default)]
struct State {
    credentials: Vec<StoredCredential>,
    blocks: HashMap<i64, i64>,
    sticky: HashMap<(String, String), i64>,
    next_id: i64,
}

#[derive(Debug, Clone, Default)]
pub struct InMemoryCredentialStore {
    state: Arc<Mutex<State>>,
}

impl CredentialStore for InMemoryCredentialStore {
    fn list<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Vec<StoredCredential>> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .credentials
                .iter()
                .filter(|row| row.provider == provider)
                .cloned()
                .collect()
        })
    }
    fn upsert<'a>(
        &'a self,
        provider: &'a str,
        kind: CredentialKind,
        identity_key: &'a str,
        label: &'a str,
    ) -> BoxFuture<'a, i64> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            if let Some(row) = state
                .credentials
                .iter_mut()
                .find(|row| row.provider == provider && row.identity_key == identity_key)
            {
                row.kind = kind;
                row.label = label.to_owned();
                row.disabled_cause = None;
                return row.id;
            }
            state.next_id += 1;
            let id = state.next_id;
            state.credentials.push(StoredCredential {
                id,
                provider: provider.to_owned(),
                kind,
                identity_key: identity_key.to_owned(),
                label: label.to_owned(),
                disabled_cause: None,
            });
            id
        })
    }
    fn update_kind<'a>(&'a self, id: i64, kind: CredentialKind) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(row) = self
                .state
                .lock()
                .await
                .credentials
                .iter_mut()
                .find(|row| row.id == id)
            {
                row.kind = kind;
            }
        })
    }
    fn disable<'a>(&'a self, id: i64, cause: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Some(row) = self
                .state
                .lock()
                .await
                .credentials
                .iter_mut()
                .find(|row| row.id == id)
            {
                row.disabled_cause = Some(cause.to_owned());
            }
        })
    }
    fn delete<'a>(&'a self, id: i64) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut state = self.state.lock().await;
            state.credentials.retain(|row| row.id != id);
            state.blocks.remove(&id);
            state.sticky.retain(|_, value| *value != id);
        })
    }
    fn blocks<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Vec<(i64, i64)>> {
        Box::pin(async move {
            let state = self.state.lock().await;
            state
                .credentials
                .iter()
                .filter(|row| row.provider == provider)
                .filter_map(|row| state.blocks.get(&row.id).map(|until| (row.id, *until)))
                .collect()
        })
    }
    fn set_block<'a>(&'a self, id: i64, until_ms: i64) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.state.lock().await.blocks.insert(id, until_ms);
        })
    }
    fn sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, Option<i64>> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .sticky
                .get(&(conversation_id.to_owned(), provider.to_owned()))
                .copied()
        })
    }
    fn set_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
        id: i64,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .sticky
                .insert((conversation_id.to_owned(), provider.to_owned()), id);
        })
    }
    fn clear_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.state
                .lock()
                .await
                .sticky
                .remove(&(conversation_id.to_owned(), provider.to_owned()));
        })
    }
}
