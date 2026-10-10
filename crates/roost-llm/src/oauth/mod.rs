//! Interactive OAuth state machine for Anthropic and OpenAI Codex accounts.
//! The coordinator presents notices and prompts to the client, then feeds prompt
//! responses back; successful sessions produce credentials for the account store.

mod anthropic;
mod codex;
mod pkce;

use serde::{Deserialize, Serialize};

use crate::{credentials::CredentialKind, endpoints::Endpoints, error::LlmError};

pub(super) use pkce::generate_pkce;
pub use pkce::parse_authorization_input;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginStatus {
    Waiting,
    Prompt,
    Done,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginPrompt {
    pub id: String,
    #[serde(rename = "type")]
    pub prompt_type: String,
    pub message: String,
    pub options: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginNotice {
    #[serde(rename = "type")]
    pub notice_type: String,
    pub message: String,
    pub url: Option<String>,
    pub code: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoginView {
    pub state: LoginStatus,
    pub prompt: Option<LoginPrompt>,
    pub notices: Vec<LoginNotice>,
    pub error: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewCredential {
    pub provider: String,
    pub kind: CredentialKind,
    pub identity_key: String,
    pub label: String,
}

#[derive(Debug)]
pub struct LoginSession {
    provider: String,
    http: reqwest::Client,
    endpoints: Endpoints,
    view: LoginView,
    verifier: Option<String>,
    state_token: Option<String>,
    device_auth_id: Option<String>,
    user_code: Option<String>,
}
impl LoginSession {
    pub async fn start(
        provider: &str,
        http: reqwest::Client,
        endpoints: Endpoints,
    ) -> Result<Self, LlmError> {
        let mut session = Self {
            provider: provider.into(),
            http,
            endpoints,
            view: LoginView {
                state: LoginStatus::Waiting,
                prompt: None,
                notices: Vec::new(),
                error: None,
            },
            verifier: None,
            state_token: None,
            device_auth_id: None,
            user_code: None,
        };
        match provider {
            "anthropic" => anthropic::start(&mut session)?,
            "openai-codex" => codex::start(&mut session).await?,
            _ => {
                return Err(LlmError::Auth(format!(
                    "OAuth login unsupported for {provider}"
                )));
            }
        }
        Ok(session)
    }
    pub fn state(&self) -> LoginView {
        self.view.clone()
    }
    pub async fn respond(
        &mut self,
        prompt_id: &str,
        value: &str,
    ) -> Result<Option<NewCredential>, LlmError> {
        let prompt = self
            .view
            .prompt
            .as_ref()
            .ok_or_else(|| LlmError::Auth("login is not waiting for a prompt".into()))?;
        if prompt.id != prompt_id {
            return Err(LlmError::Auth("login prompt id does not match".into()));
        }
        let result = match self.provider.as_str() {
            "anthropic" => anthropic::respond(self, value).await,
            "openai-codex" => codex::respond(self, value).await,
            _ => Err(LlmError::Auth("unsupported OAuth provider".into())),
        };
        match result {
            Ok(credential) => {
                self.view.state = LoginStatus::Done;
                self.view.prompt = None;
                Ok(Some(credential))
            }
            Err(error) => {
                self.view.state = LoginStatus::Failed;
                self.view.error = Some(error.to_string());
                Err(error)
            }
        }
    }
}

pub fn api_key_identity(key: &str) -> (String, String) {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(key.as_bytes());
    let fingerprint = hex::encode(digest);
    let suffix = key
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    (
        format!("key:{}", &fingerprint[..16]),
        format!("API key …{suffix}"),
    )
}

fn add_prompt(session: &mut LoginSession, id: &str, kind: &str, message: &str) {
    session.view.state = LoginStatus::Prompt;
    session.view.prompt = Some(LoginPrompt {
        id: id.into(),
        prompt_type: kind.into(),
        message: message.into(),
        options: Vec::new(),
    });
}
fn add_notice(
    session: &mut LoginSession,
    kind: &str,
    message: &str,
    url: Option<String>,
    code: Option<String>,
) {
    session.view.notices.push(LoginNotice {
        notice_type: kind.into(),
        message: message.into(),
        url,
        code,
    });
}
fn token_fields(value: &serde_json::Value) -> Result<(String, String, i64), LlmError> {
    let access = value
        .get("access_token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LlmError::Decode("OAuth response missing access_token".into()))?
        .to_owned();
    let refresh = value
        .get("refresh_token")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LlmError::Decode("OAuth response missing refresh_token".into()))?
        .to_owned();
    let expires = value
        .get("expires_in")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| LlmError::Decode("OAuth response missing expires_in".into()))?;
    Ok((
        access,
        refresh,
        super::pool::now_ms().saturating_add(expires.saturating_mul(1000)),
    ))
}
