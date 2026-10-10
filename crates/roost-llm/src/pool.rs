//! Credential selection, usage-aware ranking, and OAuth refresh for provider calls.
//! `roost-agent` resolves through this pool before each request; persistence is
//! supplied by the coordinator or the in-memory implementation.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use reqwest::Client;
use tokio::sync::Mutex;

use crate::usage::{UsageCache, UsageReport};
use crate::{
    auth::ResolvedAuth,
    credentials::{CredentialKind, CredentialStore, StoredCredential},
    endpoints::Endpoints,
    error::LlmError,
};

pub struct AccountPool {
    store: Arc<dyn CredentialStore>,
    http: Client,
    endpoints: Endpoints,
    refresh_locks: Mutex<HashMap<i64, Arc<Mutex<()>>>>,
    usage: Arc<UsageCache>,
}

impl std::fmt::Debug for AccountPool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AccountPool")
            .field("endpoints", &self.endpoints)
            .finish_non_exhaustive()
    }
}

impl AccountPool {
    pub fn new(store: Arc<dyn CredentialStore>, http: Client, endpoints: Endpoints) -> Self {
        Self {
            store,
            http,
            endpoints,
            refresh_locks: Mutex::new(HashMap::new()),
            usage: Arc::new(UsageCache::default()),
        }
    }
    pub fn store(&self) -> &Arc<dyn CredentialStore> {
        &self.store
    }
    pub async fn has_enabled_credential(&self, provider: &str) -> bool {
        self.store
            .list(provider)
            .await
            .iter()
            .any(|row| row.disabled_cause.is_none())
    }
    pub fn usage_reports(&self) -> Vec<UsageReport> {
        self.usage.reports()
    }
    pub fn header_observer(&self, credential_id: i64) -> crate::providers::HeaderObserver {
        let report = self.usage.get(credential_id);
        crate::usage::header_observer(
            self.usage.clone(),
            credential_id,
            report
                .as_ref()
                .map_or_else(|| "anthropic".into(), |item| item.provider.clone()),
            report.map_or_else(String::new, |item| item.label),
        )
    }
    pub async fn refresh_usage(&self, credential_id: i64, provider: &str) -> Option<UsageReport> {
        if let Some(report) = self
            .usage
            .get(credential_id)
            .filter(|report| now_ms().saturating_sub(report.fetched_ms) < 60_000)
        {
            return Some(report);
        }
        let fetched = crate::usage::fetch_usage(
            self.store.as_ref(),
            &self.http,
            &self.endpoints,
            credential_id,
            provider,
        )
        .await?;
        let report = if fetched.note.is_some() {
            self.usage
                .get(credential_id)
                .map(|mut previous| {
                    previous.note = fetched.note.clone();
                    previous.fetched_ms = now_ms();
                    previous
                })
                .unwrap_or(fetched)
        } else {
            fetched
        };
        self.usage.insert(report.clone());
        Some(report)
    }

    pub async fn resolve(
        &self,
        provider: &str,
        conversation_id: &str,
    ) -> Result<(i64, ResolvedAuth), LlmError> {
        let rows = self.store.list(provider).await;
        let blocked: HashMap<i64, i64> = self.store.blocks(provider).await.into_iter().collect();
        let now = now_ms();
        let candidates: Vec<_> = rows
            .iter()
            .filter(|row| row.disabled_cause.is_none())
            .collect();
        let sticky_row = self
            .store
            .sticky(conversation_id, provider)
            .await
            .and_then(|sticky_id| {
                candidates
                    .iter()
                    .find(|row| {
                        row.id == sticky_id
                            && blocked.get(&sticky_id).is_none_or(|until| *until <= now)
                    })
                    .copied()
            });
        let mut failed_sticky_id = None;
        if let Some(row) = sticky_row {
            match self.resolve_auth(row).await {
                Ok(auth) => return Ok((row.id, auth)),
                Err(error) => {
                    tracing::warn!(credential_id = row.id, %error, "sticky credential refresh failed; disabling account");
                    self.store.disable(row.id, "refresh_failed").await;
                    failed_sticky_id = Some(row.id);
                }
            }
        }
        let enabled_count = candidates.len();
        let mut eligible: Vec<_> = candidates
            .into_iter()
            .filter(|row| Some(row.id) != failed_sticky_id)
            .filter(|row| blocked.get(&row.id).is_none_or(|until| *until <= now))
            .collect();
        if eligible.is_empty() {
            let earliest = blocked.values().copied().filter(|until| *until > now).min();
            if enabled_count > 0
                && let Some(until) = earliest
            {
                return Err(LlmError::RateLimited {
                    retry_after_ms: Some(until.saturating_sub(now).max(0) as u64),
                    reset_at_ms: Some(until),
                });
            }
            return Err(LlmError::NoCredential {
                provider: provider.to_owned(),
            });
        }
        let reports = self.usage.reports();
        let rotation_start =
            xxhash_rust::xxh32::xxh32(conversation_id.as_bytes(), 0) as usize % eligible.len();
        eligible.rotate_left(rotation_start);
        eligible.sort_by(|left, right| rank(left, right, &reports));
        for row in eligible {
            match self.resolve_auth(row).await {
                Ok(auth) => {
                    self.store
                        .set_sticky(conversation_id, provider, row.id)
                        .await;
                    return Ok((row.id, auth));
                }
                Err(error) => {
                    tracing::warn!(credential_id = row.id, %error, "credential refresh failed; disabling account");
                    self.store.disable(row.id, "refresh_failed").await;
                }
            }
        }
        Err(LlmError::NoCredential {
            provider: provider.to_owned(),
        })
    }

    async fn resolve_auth(&self, row: &StoredCredential) -> Result<ResolvedAuth, LlmError> {
        match &row.kind {
            CredentialKind::ApiKey { key } => Ok(ResolvedAuth::ApiKey { key: key.clone() }),
            CredentialKind::OAuth {
                access,
                expires_ms,
                account_id,
                ..
            } => {
                if *expires_ms > now_ms() + 60_000 {
                    return Ok(ResolvedAuth::OAuth {
                        access_token: access.clone(),
                        account_id: account_id.clone(),
                    });
                }
                let lock = {
                    let mut locks = self.refresh_locks.lock().await;
                    locks
                        .entry(row.id)
                        .or_insert_with(|| Arc::new(Mutex::new(())))
                        .clone()
                };
                let _guard = lock.lock().await;
                let latest = self
                    .store
                    .list(&row.provider)
                    .await
                    .into_iter()
                    .find(|credential| credential.id == row.id)
                    .ok_or_else(|| LlmError::Auth("credential was removed".into()))?;
                if latest.disabled_cause.is_some() {
                    return Err(LlmError::Auth("credential is disabled".into()));
                }
                let CredentialKind::OAuth {
                    access,
                    refresh,
                    expires_ms,
                    account_id,
                    email,
                } = latest.kind
                else {
                    return Err(LlmError::Auth(
                        "credential kind changed during refresh".into(),
                    ));
                };
                if expires_ms > now_ms() + 60_000 {
                    return Ok(ResolvedAuth::OAuth {
                        access_token: access,
                        account_id,
                    });
                }
                let updated = self
                    .refresh(&row.provider, &refresh, account_id, email)
                    .await?;
                let auth = match &updated {
                    CredentialKind::OAuth {
                        access, account_id, ..
                    } => ResolvedAuth::OAuth {
                        access_token: access.clone(),
                        account_id: account_id.clone(),
                    },
                    CredentialKind::ApiKey { .. } => {
                        return Err(LlmError::Auth(
                            "refresh returned a non-OAuth credential".into(),
                        ));
                    }
                };
                self.store.update_kind(row.id, updated).await;
                tracing::info!(credential_id = row.id, provider = %row.provider, "OAuth credential refreshed");
                Ok(auth)
            }
        }
    }

    async fn refresh(
        &self,
        provider: &str,
        refresh: &str,
        account_id: Option<String>,
        email: Option<String>,
    ) -> Result<CredentialKind, LlmError> {
        let response = if provider == "anthropic" {
            let base = self
                .endpoints
                .base("anthropic-console", "https://platform.claude.com");
            self.http
                .post(format!("{base}/v1/oauth/token"))
                .json(&serde_json::json!({
                    "grant_type": "refresh_token",
                    "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
                    "refresh_token": refresh,
                }))
                .send()
                .await?
        } else if provider == "openai-codex" {
            let base = self
                .endpoints
                .base("openai-auth", "https://auth.openai.com");
            let body = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("grant_type", "refresh_token")
                .append_pair("refresh_token", refresh)
                .append_pair("client_id", "app_EMoamEEZ73f0CkXaXp7hrann")
                .finish();
            self.http
                .post(format!("{base}/oauth/token"))
                .header("content-type", "application/x-www-form-urlencoded")
                .body(body)
                .send()
                .await?
        } else {
            return Err(LlmError::Auth(format!(
                "OAuth refresh unsupported for {provider}"
            )));
        };
        let status = response.status();
        let bytes = response.bytes().await.map_err(LlmError::from)?;
        if !status.is_success() {
            return Err(LlmError::Auth(format!(
                "OAuth refresh HTTP {status}: {}",
                String::from_utf8_lossy(&bytes)
            )));
        }
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|error| LlmError::Decode(error.to_string()))?;
        let access = value
            .get("access_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LlmError::Decode("refresh response missing access_token".into()))?
            .to_owned();
        let refresh = value
            .get("refresh_token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| LlmError::Decode("refresh response missing refresh_token".into()))?
            .to_owned();
        let seconds = value
            .get("expires_in")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| LlmError::Decode("refresh response missing expires_in".into()))?;
        Ok(CredentialKind::OAuth {
            access,
            refresh,
            expires_ms: now_ms().saturating_add(seconds.saturating_mul(1000)),
            account_id,
            email,
        })
    }
}

fn rank(
    left: &StoredCredential,
    right: &StoredCredential,
    reports: &[UsageReport],
) -> std::cmp::Ordering {
    let report_for = |id| reports.iter().find(|report| report.credential_id == id);
    let hot = |report: Option<&UsageReport>| {
        report.is_some_and(|report| {
            report
                .windows
                .iter()
                .any(|window| window.used_fraction >= 0.90)
        })
    };
    let primary_usage = |report: Option<&UsageReport>| {
        report
            .and_then(|report| {
                report
                    .windows
                    .iter()
                    .find(|window| matches!(window.name.as_str(), "5h" | "primary"))
            })
            .map_or(0.0, |window| window.used_fraction)
    };
    let left_report = report_for(left.id);
    let right_report = report_for(right.id);
    hot(left_report)
        .cmp(&hot(right_report))
        .then_with(|| primary_usage(left_report).total_cmp(&primary_usage(right_report)))
}
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            elapsed.as_millis().min(i64::MAX as u128) as i64
        })
}
