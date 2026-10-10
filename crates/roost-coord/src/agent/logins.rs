//! Live provider sign-ins, keyed by a random login id. Anthropic waits for the
//! pasted code through `respond`; Codex's device flow polls in a background
//! task. A finished login stores its account and expires 10 minutes later.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use roost_llm::oauth::LoginStatus;
use roost_llm::{AccountPool, Endpoints, LoginSession, LoginView, NewCredential};

const SETTLED_TTL: Duration = Duration::from_secs(600);
const DEVICE_PROMPT: &str = "device_code";

struct LoginEntry {
    session: Arc<tokio::sync::Mutex<LoginSession>>,
    view: Arc<Mutex<LoginView>>,
    started: Instant,
    poller: Option<tokio::task::AbortHandle>,
}

#[derive(Default)]
pub struct LoginRegistry {
    logins: Mutex<HashMap<String, LoginEntry>>,
}

impl std::fmt::Debug for LoginRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginRegistry")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("login {0} not found")]
    NotFound(String),
    #[error("{0}")]
    Provider(String),
}

impl LoginRegistry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, LoginEntry>> {
        self.logins.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a sign-in and returns its id.
    pub async fn start(
        &self,
        provider: &str,
        http: reqwest::Client,
        endpoints: Endpoints,
        pool: Arc<AccountPool>,
    ) -> Result<String, LoginError> {
        self.expire();
        let session = LoginSession::start(provider, http, endpoints)
            .await
            .map_err(|error| LoginError::Provider(error.to_string()))?;
        let mut initial = session.state();
        let device_flow = initial
            .prompt
            .as_ref()
            .is_some_and(|prompt| prompt.id == DEVICE_PROMPT);
        if device_flow {
            // The device flow needs no input: the coordinator polls, so the
            // browser sees a waiting login with the code notice only.
            initial.prompt = None;
            initial.state = LoginStatus::Waiting;
        }
        let login_id = crate::agent::random_hex("login-");
        let session = Arc::new(tokio::sync::Mutex::new(session));
        let view = Arc::new(Mutex::new(initial));
        let poller = device_flow.then(|| {
            let session = Arc::clone(&session);
            let view = Arc::clone(&view);
            tokio::spawn(async move {
                let mut session = session.lock().await;
                let outcome = session.respond(DEVICE_PROMPT, "").await;
                finish(&session, &view, outcome, &pool).await;
            })
            .abort_handle()
        });
        tracing::info!(provider, login_id = %login_id, "agent provider login started");
        self.lock().insert(
            login_id.clone(),
            LoginEntry {
                session,
                view,
                started: Instant::now(),
                poller,
            },
        );
        Ok(login_id)
    }

    pub fn state(&self, login_id: &str) -> Result<LoginView, LoginError> {
        let logins = self.lock();
        let entry = logins
            .get(login_id)
            .ok_or_else(|| LoginError::NotFound(login_id.to_owned()))?;
        Ok(entry
            .view
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }

    /// Feeds a prompt answer (the pasted code) and stores the account on success.
    pub async fn respond(
        &self,
        login_id: &str,
        prompt_id: &str,
        value: &str,
        pool: &Arc<AccountPool>,
    ) -> Result<(), LoginError> {
        let (session, view) = {
            let logins = self.lock();
            let entry = logins
                .get(login_id)
                .ok_or_else(|| LoginError::NotFound(login_id.to_owned()))?;
            (Arc::clone(&entry.session), Arc::clone(&entry.view))
        };
        let mut session = session.lock().await;
        let outcome = session.respond(prompt_id, value).await;
        let failed = outcome.as_ref().err().map(ToString::to_string);
        finish(&session, &view, outcome, pool).await;
        match failed {
            Some(error) => Err(LoginError::Provider(error)),
            None => Ok(()),
        }
    }

    pub fn cancel(&self, login_id: &str) {
        if let Some(entry) = self.lock().remove(login_id)
            && let Some(poller) = entry.poller
        {
            poller.abort();
        }
    }

    fn expire(&self) {
        self.lock().retain(|_, entry| {
            let settled = matches!(
                entry
                    .view
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .state,
                LoginStatus::Done | LoginStatus::Failed
            );
            !(settled && entry.started.elapsed() > SETTLED_TTL)
                && entry.started.elapsed() < SETTLED_TTL * 3
        });
    }
}

async fn finish(
    session: &LoginSession,
    view: &Mutex<LoginView>,
    outcome: Result<Option<NewCredential>, roost_llm::LlmError>,
    pool: &Arc<AccountPool>,
) {
    let mut latest = session.state();
    match outcome {
        Ok(Some(credential)) => {
            let id = pool
                .store()
                .upsert(
                    &credential.provider,
                    credential.kind,
                    &credential.identity_key,
                    &credential.label,
                )
                .await;
            tracing::info!(provider = %credential.provider, credential_id = id, "agent provider account signed in");
        }
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(%error, "agent provider login failed");
            latest.state = LoginStatus::Failed;
            latest.error.get_or_insert_with(|| error.to_string());
        }
    }
    *view.lock().unwrap_or_else(PoisonError::into_inner) = latest;
}
