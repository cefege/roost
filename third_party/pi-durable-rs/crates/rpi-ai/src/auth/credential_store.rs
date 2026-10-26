//! Port of `packages/ai/src/auth/credential-store.ts` @ pi 0.82.1 (2efa728).
//!
//! Default in-memory credential store. Writes are serialized per provider
//! (upstream: promise chain; here: a per-provider async mutex).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::resolve::ModelsError;
use super::types::{AuthOperationOptions, Credential, CredentialInfo, CredentialStore, ModifyFn};

/// `InMemoryCredentialStore` — apps inject persistent stores (T04).
#[derive(Default)]
pub struct InMemoryCredentialStore {
    credentials: Mutex<HashMap<String, Credential>>,
    /// Serialize `modify`/`delete` per provider id.
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    async fn provider_lock(&self, provider_id: &str) -> Arc<Mutex<()>> {
        self.locks
            .lock()
            .await
            .entry(provider_id.to_owned())
            .or_default()
            .clone()
    }
}

#[async_trait::async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn read(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<Credential>, ModelsError> {
        // credential-store.ts:31 `options?.signal?.throwIfAborted()`.
        AuthOperationOptions::throw_if_cancelled(options)?;
        Ok(self.credentials.lock().await.get(provider_id).cloned())
    }

    async fn list(
        &self,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Vec<CredentialInfo>, ModelsError> {
        // credential-store.ts:36 `options?.signal?.throwIfAborted()`.
        AuthOperationOptions::throw_if_cancelled(options)?;
        Ok(self
            .credentials
            .lock()
            .await
            .iter()
            .map(|(provider_id, credential)| CredentialInfo {
                provider_id: provider_id.clone(),
                credential_type: credential.credential_type(),
            })
            .collect())
    }

    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyFn,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<Credential>, ModelsError> {
        // T21b: a cancelled modify queued behind another mutation rejects
        // without ever running its task (credential-store.ts:14-28 @ 4181f66).
        AuthOperationOptions::throw_if_cancelled(options)?;
        let lock = self.provider_lock(provider_id).await;
        // Queue-cancellable: cancel during lock-wait rejects without running
        // the task; after lock acquisition, re-check before executing.
        let _guard = tokio::select! {
            biased;
            _ = cancel_future(options) => return Err(ModelsError::aborted()),
            guard = lock.lock() => guard,
        };
        AuthOperationOptions::throw_if_cancelled(options)?;
        let current = self.credentials.lock().await.get(provider_id).cloned();
        let next = f(current.clone()).await?;
        // credential-store.ts:50: re-check after the callback returns, before
        // writing — a cancelled modify must not persist its result.
        AuthOperationOptions::throw_if_cancelled(options)?;
        if let Some(next) = next {
            self.credentials
                .lock()
                .await
                .insert(provider_id.to_owned(), next);
        }
        // Upstream resolves with `next ?? current`.
        Ok(self.credentials.lock().await.get(provider_id).cloned())
    }

    async fn delete(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<(), ModelsError> {
        // T21b: a cancelled delete queued behind another mutation rejects
        // without ever running its task (credential-store.ts:14-28 @ 4181f66).
        AuthOperationOptions::throw_if_cancelled(options)?;
        let lock = self.provider_lock(provider_id).await;
        // Queue-cancellable: cancel during lock-wait rejects without running
        // the task; after lock acquisition, re-check before executing.
        let _guard = tokio::select! {
            biased;
            _ = cancel_future(options) => return Err(ModelsError::aborted()),
            guard = lock.lock() => guard,
        };
        AuthOperationOptions::throw_if_cancelled(options)?;
        self.credentials.lock().await.remove(provider_id);
        Ok(())
    }
}

/// Boxed future that resolves when the cancellation token in `options` fires,
/// or never when there is no token. Used in `select!` to cancel a queued
/// `modify`/`delete` while it waits for the per-provider lock.
pub(crate) fn cancel_future(
    options: Option<&AuthOperationOptions>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
    match options.and_then(|o| o.signal.as_ref()) {
        Some(token) => Box::pin(token.cancelled()),
        None => Box::pin(std::future::pending()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::resolve::ModelsErrorCode;
    use crate::auth::types::ApiKeyCredential;
    use tokio_util::sync::CancellationToken;

    fn api_key_credential(key: &str) -> Credential {
        Credential::ApiKey(ApiKeyCredential {
            key: Some(key.to_owned()),
            env: None,
        })
    }

    fn set_fn(credential: Credential) -> ModifyFn {
        Arc::new(move |_| {
            let credential = credential.clone();
            Box::pin(async move { Ok(Some(credential)) })
        })
    }

    /// read/list honor the entry `throwIfAborted`
    /// (credential-store.ts:31/36 @ 4181f66).
    #[tokio::test]
    async fn read_and_list_reject_a_cancelled_signal() {
        let store = InMemoryCredentialStore::new();
        store
            .modify("anthropic", set_fn(api_key_credential("k")), None)
            .await
            .expect("seed");
        let token = CancellationToken::new();
        token.cancel();
        let options = AuthOperationOptions::with_signal(token);
        let error = store
            .read("anthropic", Some(&options))
            .await
            .expect_err("read must reject");
        assert_eq!(error.code, ModelsErrorCode::Aborted);
        let error = store
            .list(Some(&options))
            .await
            .expect_err("list must reject");
        assert_eq!(error.code, ModelsErrorCode::Aborted);
    }

    /// Cancellation during the modify callback rejects before the write
    /// (credential-store.ts:50 @ 4181f66): the new credential is not stored.
    #[tokio::test]
    async fn modify_cancelled_after_callback_does_not_write() {
        let store = InMemoryCredentialStore::new();
        let token = CancellationToken::new();
        let cancel_in_fn = token.clone();
        let f: ModifyFn = Arc::new(move |_| {
            let token = cancel_in_fn.clone();
            Box::pin(async move {
                token.cancel();
                Ok(Some(api_key_credential("new")))
            })
        });
        let options = AuthOperationOptions::with_signal(token);
        let error = store
            .modify("anthropic", f, Some(&options))
            .await
            .expect_err("modify must reject");
        assert_eq!(error.code, ModelsErrorCode::Aborted);
        assert!(store.read("anthropic", None).await.expect("read").is_none());
    }
}
