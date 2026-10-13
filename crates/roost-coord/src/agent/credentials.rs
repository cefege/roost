//! Coordinator-owned credentials, rate-limit blocks, and account affinity.
//!
//! The store encrypts provider secrets at rest and serializes OAuth refreshes
//! across coordinator replicas using a per-credential PostgreSQL advisory lock.

mod credential_crypto;

use credential_crypto::{credential_kind_name, decode_credential, encode_data};

use futures::future::BoxFuture;
use roost_llm::credentials::{
    CredentialKind, CredentialStore, CredentialStoreError, RefreshOperation, StoredCredential,
};
use sqlx::{Row as _, any::AnyRow};

use crate::db::{CoordDb, DbBackend};

const CREDENTIAL_BY_ID: &str = "SELECT id,provider,kind,identity_key,label,data_json,disabled_cause \
     FROM agent_credentials WHERE id=$1";
const CREDENTIALS_BY_PROVIDER: &str = "SELECT id,provider,kind,identity_key,label,data_json,disabled_cause \
     FROM agent_credentials WHERE provider=$1 ORDER BY id";

#[derive(Debug, Clone)]
pub struct CoordCredentialStore {
    db: CoordDb,
    encryption_key: Option<[u8; 32]>,
}

impl CoordCredentialStore {
    #[must_use]
    pub fn new(db: CoordDb, encryption_key: Option<[u8; 32]>) -> Self {
        Self { db, encryption_key }
    }

    async fn decode_rows(
        &self,
        rows: Vec<AnyRow>,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    ) -> Result<Vec<StoredCredential>, CredentialStoreError> {
        let mut credentials = Vec::with_capacity(rows.len());
        for row in rows {
            let (credential, needs_reencrypt) =
                decode_credential(self.encryption_key.as_ref(), &row)?;
            if needs_reencrypt {
                let data_json = encode_data(
                    self.encryption_key.as_ref(),
                    &credential.provider,
                    &credential.identity_key,
                    &credential.kind,
                )?;
                sqlx::query("UPDATE agent_credentials SET data_json=$1,updated_ms=$2 WHERE id=$3")
                    .bind(data_json)
                    .bind(now_ms())
                    .bind(credential.id)
                    .execute(&mut **transaction)
                    .await
                    .map_err(storage_error)?;
            }
            credentials.push(credential);
        }
        Ok(credentials)
    }

    async fn read_credential(
        &self,
        id: i64,
        transaction: &mut sqlx::Transaction<'_, sqlx::Any>,
    ) -> Result<Option<(StoredCredential, bool)>, CredentialStoreError> {
        let row = sqlx::query(CREDENTIAL_BY_ID)
            .bind(id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(storage_error)?;
        row.as_ref()
            .map(|row| decode_credential(self.encryption_key.as_ref(), row))
            .transpose()
    }
}

impl CredentialStore for CoordCredentialStore {
    fn list<'a>(
        &'a self,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Vec<StoredCredential>, CredentialStoreError>> {
        Box::pin(async move {
            let rows = sqlx::query(CREDENTIALS_BY_PROVIDER)
                .bind(provider)
                .fetch_all(self.db.pool())
                .await
                .map_err(storage_error)?;
            let has_legacy_rows = self.encryption_key.is_some()
                && rows.iter().any(|row| {
                    row.try_get::<String, _>("data_json").is_ok_and(|json| {
                        serde_json::from_str::<serde_json::Value>(&json)
                            .is_ok_and(|value| value.get("v").is_none())
                    })
                });
            if !has_legacy_rows {
                return rows
                    .iter()
                    .map(|row| {
                        decode_credential(self.encryption_key.as_ref(), row)
                            .map(|(credential, _)| credential)
                    })
                    .collect();
            }
            let mut transaction = self.db.begin_write().await.map_err(storage_error)?;
            let rows = sqlx::query(CREDENTIALS_BY_PROVIDER)
                .bind(provider)
                .fetch_all(&mut *transaction)
                .await
                .map_err(storage_error)?;
            let credentials = self.decode_rows(rows, &mut transaction).await?;
            transaction.commit().await.map_err(storage_error)?;
            Ok(credentials)
        })
    }

    fn upsert<'a>(
        &'a self,
        provider: &'a str,
        kind: CredentialKind,
        identity_key: &'a str,
        label: &'a str,
    ) -> BoxFuture<'a, Result<i64, CredentialStoreError>> {
        Box::pin(async move {
            let data_json =
                encode_data(self.encryption_key.as_ref(), provider, identity_key, &kind)?;
            let now = now_ms();
            let id = sqlx::query_scalar::<_, i64>("INSERT INTO agent_credentials(provider,kind,identity_key,label,data_json,disabled_cause,created_ms,updated_ms) VALUES($1,$2,$3,$4,$5,NULL,$6,$6) ON CONFLICT(provider,identity_key) DO UPDATE SET kind=excluded.kind,label=excluded.label,data_json=excluded.data_json,disabled_cause=NULL,updated_ms=excluded.updated_ms RETURNING id")
                .bind(provider)
                .bind(credential_kind_name(&kind))
                .bind(identity_key)
                .bind(label)
                .bind(data_json)
                .bind(now)
                .fetch_one(self.db.pool())
                .await
                .map_err(storage_error)?;
            Ok(id)
        })
    }

    fn update_kind<'a>(
        &'a self,
        id: i64,
        kind: CredentialKind,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>> {
        Box::pin(async move {
            let identity: Option<(String, String)> =
                sqlx::query_as("SELECT provider,identity_key FROM agent_credentials WHERE id=$1")
                    .bind(id)
                    .fetch_optional(self.db.pool())
                    .await
                    .map_err(storage_error)?;
            let Some((provider, identity_key)) = identity else {
                return Ok(());
            };
            let data_json = encode_data(
                self.encryption_key.as_ref(),
                &provider,
                &identity_key,
                &kind,
            )?;
            sqlx::query(
                "UPDATE agent_credentials SET kind=$1,data_json=$2,updated_ms=$3 WHERE id=$4",
            )
            .bind(credential_kind_name(&kind))
            .bind(data_json)
            .bind(now_ms())
            .bind(id)
            .execute(self.db.pool())
            .await
            .map_err(storage_error)?;
            Ok(())
        })
    }

    fn disable<'a>(
        &'a self,
        id: i64,
        cause: &'a str,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>> {
        Box::pin(async move {
            sqlx::query("UPDATE agent_credentials SET disabled_cause=$1,updated_ms=$2 WHERE id=$3")
                .bind(cause)
                .bind(now_ms())
                .bind(id)
                .execute(self.db.pool())
                .await
                .map_err(storage_error)?;
            Ok(())
        })
    }

    fn clear_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>> {
        Box::pin(async move {
            sqlx::query(
                "DELETE FROM agent_credential_sticky WHERE conversation_id=$1 AND provider=$2",
            )
            .bind(conversation_id)
            .bind(provider)
            .execute(self.db.pool())
            .await
            .map_err(storage_error)?;
            Ok(())
        })
    }

    fn delete<'a>(&'a self, id: i64) -> BoxFuture<'a, Result<(), CredentialStoreError>> {
        Box::pin(async move {
            let mut transaction = self.db.begin_write().await.map_err(storage_error)?;
            sqlx::query("DELETE FROM agent_credential_sticky WHERE credential_id=$1")
                .bind(id)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            sqlx::query("DELETE FROM agent_credential_blocks WHERE credential_id=$1")
                .bind(id)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            sqlx::query("DELETE FROM agent_credentials WHERE id=$1")
                .bind(id)
                .execute(&mut *transaction)
                .await
                .map_err(storage_error)?;
            transaction.commit().await.map_err(storage_error)?;
            Ok(())
        })
    }

    fn blocks<'a>(
        &'a self,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Vec<(i64, i64)>, CredentialStoreError>> {
        Box::pin(async move {
            sqlx::query_as::<_, (i64, i64)>("SELECT b.credential_id,b.until_ms FROM agent_credential_blocks b JOIN agent_credentials c ON c.id=b.credential_id WHERE c.provider=$1 ORDER BY b.credential_id")
                .bind(provider)
                .fetch_all(self.db.pool())
                .await
                .map_err(storage_error)
        })
    }

    fn set_block<'a>(
        &'a self,
        id: i64,
        until_ms: i64,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO agent_credential_blocks(credential_id,until_ms) VALUES($1,$2) ON CONFLICT(credential_id) DO UPDATE SET until_ms=excluded.until_ms")
                .bind(id)
                .bind(until_ms)
                .execute(self.db.pool())
                .await
                .map_err(storage_error)?;
            Ok(())
        })
    }

    fn sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, Result<Option<i64>, CredentialStoreError>> {
        Box::pin(async move {
            sqlx::query_scalar("SELECT credential_id FROM agent_credential_sticky WHERE conversation_id=$1 AND provider=$2")
                .bind(conversation_id)
                .bind(provider)
                .fetch_optional(self.db.pool())
                .await
                .map_err(storage_error)
        })
    }

    fn set_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
        id: i64,
    ) -> BoxFuture<'a, Result<(), CredentialStoreError>> {
        Box::pin(async move {
            sqlx::query("INSERT INTO agent_credential_sticky(conversation_id,provider,credential_id) VALUES($1,$2,$3) ON CONFLICT(conversation_id,provider) DO UPDATE SET credential_id=excluded.credential_id")
                .bind(conversation_id)
                .bind(provider)
                .bind(id)
                .execute(self.db.pool())
                .await
                .map_err(storage_error)?;
            Ok(())
        })
    }

    fn refresh_locked<'a>(
        &'a self,
        id: i64,
        refresh: RefreshOperation<'a>,
    ) -> BoxFuture<'a, Result<StoredCredential, roost_llm::LlmError>> {
        Box::pin(async move {
            let mut transaction = match self.db.backend() {
                DbBackend::Postgres => self.db.pool().begin().await.map_err(llm_storage_error)?,
                DbBackend::Sqlite => self.db.begin_write().await.map_err(llm_storage_error)?,
            };
            if self.db.backend() == DbBackend::Postgres {
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('cred:' || CAST($1 AS TEXT), 0))")
                    .bind(id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(llm_storage_error)?;
            }
            let Some((mut credential, needs_reencrypt)) = self
                .read_credential(id, &mut transaction)
                .await
                .map_err(roost_llm::LlmError::from)?
            else {
                transaction.rollback().await.map_err(llm_storage_error)?;
                return Err(roost_llm::LlmError::Auth("credential was removed".into()));
            };
            if needs_reencrypt {
                let data_json = encode_data(
                    self.encryption_key.as_ref(),
                    &credential.provider,
                    &credential.identity_key,
                    &credential.kind,
                )
                .map_err(roost_llm::LlmError::from)?;

                sqlx::query("UPDATE agent_credentials SET data_json=$1,updated_ms=$2 WHERE id=$3")
                    .bind(data_json)
                    .bind(now_ms())
                    .bind(credential.id)
                    .execute(&mut *transaction)
                    .await
                    .map_err(llm_storage_error)?;
            }
            let updated_kind = match refresh(credential.clone()).await {
                Ok(kind) => kind,
                Err(error) => {
                    transaction.commit().await.map_err(llm_storage_error)?;
                    return Err(error);
                }
            };
            if credential.kind != updated_kind {
                let data_json = encode_data(
                    self.encryption_key.as_ref(),
                    &credential.provider,
                    &credential.identity_key,
                    &updated_kind,
                )
                .map_err(roost_llm::LlmError::from)?;
                sqlx::query(
                    "UPDATE agent_credentials SET kind=$1,data_json=$2,updated_ms=$3 WHERE id=$4",
                )
                .bind(credential_kind_name(&updated_kind))
                .bind(data_json)
                .bind(now_ms())
                .bind(id)
                .execute(&mut *transaction)
                .await
                .map_err(llm_storage_error)?;
                credential.kind = updated_kind;
            }
            transaction.commit().await.map_err(llm_storage_error)?;
            Ok(credential)
        })
    }
}

fn storage_error(error: sqlx::Error) -> CredentialStoreError {
    CredentialStoreError::Storage(error.to_string())
}

fn llm_storage_error(error: sqlx::Error) -> roost_llm::LlmError {
    CredentialStoreError::Storage(error.to_string()).into()
}

fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(i64::MAX)
}
