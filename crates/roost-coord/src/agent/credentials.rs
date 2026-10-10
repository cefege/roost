//! Coordinator-owned credentials, rate-limit blocks, and account affinity.
//!
//! This is the database implementation of `roost_llm::CredentialStore`; secret
//! material is serialized only into `data_json` and is never included in logs.

use futures::future::BoxFuture;
use roost_llm::credentials::{CredentialKind, CredentialStore, StoredCredential};
use sqlx::Row as _;

use crate::db::CoordDb;

#[derive(Debug, Clone)]
pub struct CoordCredentialStore {
    db: CoordDb,
}

impl CoordCredentialStore {
    #[must_use]
    pub fn new(db: CoordDb) -> Self {
        Self { db }
    }
}

impl CredentialStore for CoordCredentialStore {
    fn list<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Vec<StoredCredential>> {
        Box::pin(async move {
            let result = async {
                let rows = sqlx::query("SELECT id,provider,kind,identity_key,label,data_json,disabled_cause FROM agent_credentials WHERE provider=$1 ORDER BY id")
                    .bind(provider).fetch_all(self.db.pool()).await?;
                rows.iter().map(decode_credential).collect::<Result<Vec<_>, sqlx::Error>>()
            }.await;
            result.unwrap_or_else(|error| {
                warn_failure("list", &error);
                Vec::new()
            })
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
            let result = async {
                let data_json = serde_json::to_string(&kind).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
                let now = now_ms();
                let kind_name = credential_kind_name(&kind);
                sqlx::query_scalar::<_, i64>("INSERT INTO agent_credentials(provider,kind,identity_key,label,data_json,disabled_cause,created_ms,updated_ms) VALUES($1,$2,$3,$4,$5,NULL,$6,$6) ON CONFLICT(provider,identity_key) DO UPDATE SET kind=excluded.kind,label=excluded.label,data_json=excluded.data_json,disabled_cause=NULL,updated_ms=excluded.updated_ms RETURNING id")
                    .bind(provider).bind(kind_name).bind(identity_key).bind(label).bind(data_json).bind(now)
                    .fetch_one(self.db.pool()).await
            }.await;
            result.unwrap_or_else(|error| {
                warn_failure("upsert", &error);
                0
            })
        })
    }

    fn update_kind<'a>(&'a self, id: i64, kind: CredentialKind) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let result = async {
                let data_json = serde_json::to_string(&kind)
                    .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
                sqlx::query(
                    "UPDATE agent_credentials SET kind=$1,data_json=$2,updated_ms=$3 WHERE id=$4",
                )
                .bind(credential_kind_name(&kind))
                .bind(data_json)
                .bind(now_ms())
                .bind(id)
                .execute(self.db.pool())
                .await?;
                Ok::<(), sqlx::Error>(())
            }
            .await;
            if let Err(error) = result {
                warn_failure("update_kind", &error);
            }
        })
    }

    fn disable<'a>(&'a self, id: i64, cause: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = sqlx::query(
                "UPDATE agent_credentials SET disabled_cause=$1,updated_ms=$2 WHERE id=$3",
            )
            .bind(cause)
            .bind(now_ms())
            .bind(id)
            .execute(self.db.pool())
            .await
            {
                warn_failure("disable", &error);
            }
        })
    }

    fn clear_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = sqlx::query(
                "DELETE FROM agent_credential_sticky WHERE conversation_id=$1 AND provider=$2",
            )
            .bind(conversation_id)
            .bind(provider)
            .execute(self.db.pool())
            .await
            {
                warn_failure("clear_sticky", &error);
            }
        })
    }

    fn delete<'a>(&'a self, id: i64) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let result = async {
                let mut tx = self.db.begin_write().await?;
                sqlx::query("DELETE FROM agent_credential_sticky WHERE credential_id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM agent_credential_blocks WHERE credential_id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM agent_credentials WHERE id=$1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await
            }
            .await;
            if let Err(error) = result {
                warn_failure("delete", &error);
            }
        })
    }

    fn blocks<'a>(&'a self, provider: &'a str) -> BoxFuture<'a, Vec<(i64, i64)>> {
        Box::pin(async move {
            let result = sqlx::query_as::<_, (i64, i64)>("SELECT b.credential_id,b.until_ms FROM agent_credential_blocks b JOIN agent_credentials c ON c.id=b.credential_id WHERE c.provider=$1 ORDER BY b.credential_id")
                .bind(provider).fetch_all(self.db.pool()).await;
            result.unwrap_or_else(|error| {
                warn_failure("blocks", &error);
                Vec::new()
            })
        })
    }

    fn set_block<'a>(&'a self, id: i64, until_ms: i64) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = sqlx::query("INSERT INTO agent_credential_blocks(credential_id,until_ms) VALUES($1,$2) ON CONFLICT(credential_id) DO UPDATE SET until_ms=excluded.until_ms")
                .bind(id).bind(until_ms).execute(self.db.pool()).await { warn_failure("set_block", &error); }
        })
    }

    fn sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
    ) -> BoxFuture<'a, Option<i64>> {
        Box::pin(async move {
            let result = sqlx::query_scalar("SELECT credential_id FROM agent_credential_sticky WHERE conversation_id=$1 AND provider=$2")
                .bind(conversation_id).bind(provider).fetch_optional(self.db.pool()).await;
            result.unwrap_or_else(|error| {
                warn_failure("sticky", &error);
                None
            })
        })
    }

    fn set_sticky<'a>(
        &'a self,
        conversation_id: &'a str,
        provider: &'a str,
        id: i64,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = sqlx::query("INSERT INTO agent_credential_sticky(conversation_id,provider,credential_id) VALUES($1,$2,$3) ON CONFLICT(conversation_id,provider) DO UPDATE SET credential_id=excluded.credential_id")
                .bind(conversation_id).bind(provider).bind(id).execute(self.db.pool()).await { warn_failure("set_sticky", &error); }
        })
    }
}

fn decode_credential(row: &sqlx::any::AnyRow) -> Result<StoredCredential, sqlx::Error> {
    let json: String = row.try_get("data_json")?;
    let kind: CredentialKind =
        serde_json::from_str(&json).map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
    Ok(StoredCredential {
        id: row.try_get("id")?,
        provider: row.try_get("provider")?,
        kind,
        identity_key: row.try_get("identity_key")?,
        label: row.try_get("label")?,
        disabled_cause: row.try_get("disabled_cause")?,
    })
}

fn credential_kind_name(kind: &CredentialKind) -> &'static str {
    match kind {
        CredentialKind::OAuth { .. } => "oauth",
        CredentialKind::ApiKey { .. } => "api_key",
    }
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

fn warn_failure(operation: &'static str, error: &sqlx::Error) {
    tracing::warn!(operation, error = %error, "credential store operation failed");
}
