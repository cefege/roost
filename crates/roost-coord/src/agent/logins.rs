//! Durable provider sign-ins shared by coordinator replicas.
//! Login state stores OAuth protocol data and a client-visible view; HTTP
//! clients are reconstructed on load, and Codex pollers are lease-owned.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

mod login_leases;
mod login_store;
use login_leases::{
    claim_poller_lease, login_lease_name, release_poller_lease, renew_poller_lease,
};
use roost_llm::oauth::{LoginSessionState, LoginStatus};
use roost_llm::{AccountPool, Endpoints, LoginSession, LoginView, NewCredential};
use serde::{Deserialize, Serialize};

use crate::db::CoordDb;

const SETTLED_TTL_MS: i64 = 600_000;
const PENDING_TTL_MS: i64 = 1_800_000;
const POLLER_RENEW_INTERVAL: Duration = Duration::from_secs(10);
const DEVICE_PROMPT: &str = "device_code";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LoginRecord {
    provider: String,
    session: LoginSessionState,
    view: LoginView,
    started_ms: i64,
}

struct LocalPoller {
    epoch: u64,
    abort: tokio::task::AbortHandle,
}

#[derive(Clone)]
pub struct LoginRegistry {
    db: CoordDb,
    instance_id: String,
    http: reqwest::Client,
    endpoints: Endpoints,
    pool: Arc<AccountPool>,
    pollers: Arc<tokio::sync::Mutex<HashMap<String, LocalPoller>>>,
}

impl std::fmt::Debug for LoginRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginRegistry")
            .field("instance_id", &self.instance_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("login {0} not found")]
    NotFound(String),
    #[error("{0}")]
    Provider(String),
    #[error("login storage failed: {0}")]
    Storage(String),
}

impl LoginRegistry {
    #[must_use]
    pub fn new(
        db: CoordDb,
        instance_id: String,
        http: reqwest::Client,
        endpoints: Endpoints,
        pool: Arc<AccountPool>,
    ) -> Self {
        Self {
            db,
            instance_id,
            http,
            endpoints,
            pool,
            pollers: Arc::default(),
        }
    }

    /// Starts a sign-in and returns its id.
    pub async fn start(&self, provider: &str) -> Result<String, LoginError> {
        self.expire().await?;
        let session = LoginSession::start(provider, self.http.clone(), self.endpoints.clone())
            .await
            .map_err(|error| LoginError::Provider(error.to_string()))?;
        let mut view = session.state();
        let device_flow = view
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.id == DEVICE_PROMPT);
        if device_flow {
            view.prompt = None;
            view.state = LoginStatus::Waiting;
        }
        let login_id = crate::agent::random_hex("login-");
        let record = LoginRecord {
            provider: provider.to_owned(),
            session: session.snapshot(),
            view,
            started_ms: now_ms(),
        };
        let state_json = serialize_record(&record)?;
        sqlx::query(
            "INSERT INTO agent_logins(id,provider,state_json,expires_ms) VALUES($1,$2,$3,$4)",
        )
        .bind(&login_id)
        .bind(provider)
        .bind(state_json)
        .bind(record_expiry(&record))
        .execute(self.db.pool())
        .await
        .map_err(login_storage)?;
        tracing::info!(provider, login_id = %login_id, "agent provider login started");
        if device_flow {
            self.ensure_device_poller(&login_id, record).await?;
        }
        Ok(login_id)
    }

    pub async fn state(&self, login_id: &str) -> Result<LoginView, LoginError> {
        self.expire().await?;
        let record = self.load_record(login_id).await?;
        if record.provider == "openai-codex" && record.view.state == LoginStatus::Waiting {
            self.ensure_device_poller(login_id, record.clone()).await?;
        }
        Ok(record.view)
    }

    /// Feeds a prompt answer and stores the account on success.
    pub async fn respond(
        &self,
        login_id: &str,
        prompt_id: &str,
        value: &str,
    ) -> Result<(), LoginError> {
        self.expire().await?;
        let record = self.load_record(login_id).await?;
        let mut session = LoginSession::restore(
            record.session.clone(),
            self.http.clone(),
            self.endpoints.clone(),
        );
        let outcome = session.respond(prompt_id, value).await;
        self.complete(login_id, record, &session, outcome).await
    }

    pub async fn cancel(&self, login_id: &str) -> Result<(), LoginError> {
        self.expire().await?;
        let mut transaction = self.db.begin_write().await.map_err(login_storage)?;
        sqlx::query("DELETE FROM agent_logins WHERE id=$1")
            .bind(login_id)
            .execute(&mut *transaction)
            .await
            .map_err(login_storage)?;
        sqlx::query("DELETE FROM coord_leases WHERE name=$1")
            .bind(login_lease_name(login_id))
            .execute(&mut *transaction)
            .await
            .map_err(login_storage)?;
        transaction.commit().await.map_err(login_storage)?;
        if let Some(poller) = self.pollers.lock().await.remove(login_id) {
            poller.abort.abort();
        }
        tracing::info!(login_id, "agent provider login cancelled");
        Ok(())
    }

    async fn ensure_device_poller(
        &self,
        login_id: &str,
        record: LoginRecord,
    ) -> Result<(), LoginError> {
        let mut pollers = self.pollers.lock().await;
        if pollers.contains_key(login_id) {
            return Ok(());
        }
        let Some(epoch) = claim_poller_lease(&self.db, &self.instance_id, login_id).await? else {
            return Ok(());
        };
        tracing::info!(login_id, epoch, instance_id = %self.instance_id, "agent device-code poller lease claimed");
        let registry = self.clone();
        let login_id = login_id.to_owned();
        let task_login_id = login_id.clone();
        let task = tokio::spawn(async move {
            registry
                .run_device_poller(task_login_id, record, epoch)
                .await;
        });
        pollers.insert(
            login_id,
            LocalPoller {
                epoch,
                abort: task.abort_handle(),
            },
        );
        Ok(())
    }

    async fn run_device_poller(&self, login_id: String, record: LoginRecord, epoch: u64) {
        let mut session = LoginSession::restore(
            record.session.clone(),
            self.http.clone(),
            self.endpoints.clone(),
        );
        let outcome = {
            let response = session.respond(DEVICE_PROMPT, "");
            tokio::pin!(response);
            loop {
                tokio::select! {
                    outcome = &mut response => break outcome,
                    () = tokio::time::sleep(POLLER_RENEW_INTERVAL) => {
                        match renew_poller_lease(&self.db, &self.instance_id, &login_id, epoch).await {
                            Ok(true) => {}
                            Ok(false) => {
                                tracing::warn!(login_id, epoch, instance_id = %self.instance_id, "agent device-code poller lease lost");
                                self.remove_local_poller(&login_id, epoch).await;
                                return;
                            }
                            Err(error) => {
                                tracing::warn!(login_id, %error, "agent device-code poller lease renewal failed");
                                self.remove_local_poller(&login_id, epoch).await;
                                return;
                            }
                        }
                    }
                }
            }
        };
        match renew_poller_lease(&self.db, &self.instance_id, &login_id, epoch).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!(
                    login_id,
                    epoch,
                    "agent device-code poller finished after losing its lease"
                );
                self.remove_local_poller(&login_id, epoch).await;
                return;
            }
            Err(error) => {
                tracing::warn!(login_id, %error, "agent device-code poller could not verify its lease");
                self.remove_local_poller(&login_id, epoch).await;
                return;
            }
        }
        if let Err(error) = self.complete(&login_id, record, &session, outcome).await {
            tracing::warn!(login_id, %error, "agent device-code login completion failed");
        }
        if let Err(error) =
            release_poller_lease(&self.db, &self.instance_id, &login_id, epoch).await
        {
            tracing::warn!(login_id, %error, "agent device-code poller lease release failed");
        }
        self.remove_local_poller(&login_id, epoch).await;
    }

    async fn complete(
        &self,
        login_id: &str,
        mut record: LoginRecord,
        session: &LoginSession,
        outcome: Result<Option<NewCredential>, roost_llm::LlmError>,
    ) -> Result<(), LoginError> {
        record.session = session.snapshot();
        record.view = session.state();
        let mut result = Ok(());
        match outcome {
            Ok(Some(credential)) => {
                let provider = credential.provider.clone();
                match self
                    .pool
                    .store()
                    .upsert(
                        &credential.provider,
                        credential.kind,
                        &credential.identity_key,
                        &credential.label,
                    )
                    .await
                {
                    Ok(credential_id) => {
                        tracing::info!(provider, credential_id, "agent provider account signed in")
                    }
                    Err(error) => {
                        record.view.state = LoginStatus::Failed;
                        record.view.error = Some(error.to_string());
                        result = Err(LoginError::Storage(error.to_string()));
                    }
                }
            }
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%error, "agent provider login failed");
                record.view.state = LoginStatus::Failed;
                record.view.error.get_or_insert_with(|| error.to_string());
                result = Err(LoginError::Provider(error.to_string()));
            }
        }
        self.save_record(login_id, &record).await?;
        result
    }

    async fn remove_local_poller(&self, login_id: &str, epoch: u64) {
        let mut pollers = self.pollers.lock().await;
        if pollers
            .get(login_id)
            .is_some_and(|poller| poller.epoch == epoch)
        {
            pollers.remove(login_id);
        }
    }
}

fn serialize_record(record: &LoginRecord) -> Result<String, LoginError> {
    serde_json::to_string(record).map_err(|error| LoginError::Storage(error.to_string()))
}

fn record_expiry(record: &LoginRecord) -> i64 {
    match &record.view.state {
        LoginStatus::Done | LoginStatus::Failed => now_ms().saturating_add(SETTLED_TTL_MS),
        LoginStatus::Waiting | LoginStatus::Prompt => {
            record.started_ms.saturating_add(PENDING_TTL_MS)
        }
    }
}

fn login_storage(error: sqlx::Error) -> LoginError {
    LoginError::Storage(error.to_string())
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
