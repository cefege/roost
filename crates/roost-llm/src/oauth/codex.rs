//! OpenAI Codex OAuth device-code flow, using pi-ai's device endpoints and client.
//! The device authorization code is polled until completion, then exchanged for
//! access and refresh tokens; JWT claims supply stable account identity.

use super::{LoginSession, NewCredential, add_notice, add_prompt, token_fields};
use crate::{credentials::CredentialKind, error::LlmError};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const DEVICE_CODE_TIMEOUT_SECONDS: u64 = 15 * 60;
const JWT_AUTH: &str = "https://api.openai.com/auth";
const JWT_PROFILE: &str = "https://api.openai.com/profile";

pub(super) async fn start(session: &mut LoginSession) -> Result<(), LlmError> {
    let base = session
        .endpoints
        .base("openai-auth", "https://auth.openai.com");
    let response = session
        .http
        .post(format!("{base}/api/accounts/deviceauth/usercode"))
        .json(&serde_json::json!({"client_id":CLIENT_ID}))
        .send()
        .await?;
    let status = response.status();
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| LlmError::Decode(error.to_string()))?;
    if !status.is_success() {
        return Err(LlmError::Auth(format!(
            "Codex device code HTTP {status}: {value}"
        )));
    }
    let device_id = value
        .get("device_auth_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LlmError::Decode("device code response missing device_auth_id".into()))?
        .to_owned();
    let user_code = value
        .get("user_code")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LlmError::Decode("device code response missing user_code".into()))?
        .to_owned();
    let interval = value
        .get("interval")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| value.parse::<u64>().ok())
        .or_else(|| value.get("interval").and_then(serde_json::Value::as_u64))
        .unwrap_or(5);
    session.device_auth_id = Some(device_id);
    session.user_code = Some(user_code.clone());
    session.state_token = Some(format!("{interval}"));
    add_notice(
        session,
        "device_code",
        "Enter this code on OpenAI to authorize Codex.",
        Some(format!("{base}/codex/device")),
        Some(user_code.clone()),
    );
    add_prompt(
        session,
        "device_code",
        "text",
        "After authorizing, continue to poll for the token:",
    );
    Ok(())
}

pub(super) async fn respond(
    session: &mut LoginSession,
    _value: &str,
) -> Result<NewCredential, LlmError> {
    let base = session
        .endpoints
        .base("openai-auth", "https://auth.openai.com");
    let interval = session
        .state_token
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(5)
        .max(1);
    let device_id = session
        .device_auth_id
        .as_deref()
        .ok_or_else(|| LlmError::Auth("device auth id missing".into()))?;
    let user_code = session
        .user_code
        .as_deref()
        .ok_or_else(|| LlmError::Auth("device user code missing".into()))?;
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_secs(DEVICE_CODE_TIMEOUT_SECONDS);
    let (authorization_code, verifier) = loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(LlmError::Auth("Codex device code expired".into()));
        }
        let response = session
            .http
            .post(format!("{base}/api/accounts/deviceauth/token"))
            .json(&serde_json::json!({"device_auth_id":device_id,"user_code":user_code}))
            .send()
            .await?;
        if response.status().is_success() {
            let value: serde_json::Value = response
                .json()
                .await
                .map_err(|error| LlmError::Decode(error.to_string()))?;
            let code = value
                .get("authorization_code")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    LlmError::Decode("device token missing authorization_code".into())
                })?;
            let verifier = value
                .get("code_verifier")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| LlmError::Decode("device token missing code_verifier".into()))?;
            break (code.to_owned(), verifier.to_owned());
        }
        if !matches!(response.status().as_u16(), 403 | 404) {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            if text.contains("deviceauth_authorization_pending") || text.contains("slow_down") {
                tokio::time::sleep(std::time::Duration::from_secs(
                    interval + u64::from(text.contains("slow_down")),
                ))
                .await;
                continue;
            }
            return Err(LlmError::Auth(format!(
                "Codex device poll HTTP {status}: {text}"
            )));
        }
        tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
    };
    const DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "authorization_code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("code", &authorization_code)
        .append_pair("code_verifier", &verifier)
        .append_pair("redirect_uri", DEVICE_REDIRECT_URI)
        .finish();
    let token = session
        .http
        .post(format!("{base}/oauth/token"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await?;
    let status = token.status();
    let value: serde_json::Value = token
        .json()
        .await
        .map_err(|error| LlmError::Decode(error.to_string()))?;
    if !status.is_success() {
        return Err(LlmError::Auth(format!(
            "Codex token exchange HTTP {status}: {value}"
        )));
    }
    let (access, refresh, expires_ms) = token_fields(&value)?;
    let claims = jwt_payload(&access)?;
    let auth = claims
        .get(JWT_AUTH)
        .and_then(|value| value.get("chatgpt_account_id"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LlmError::Decode("Codex access token missing account id".into()))?
        .to_owned();
    let email = claims
        .get(JWT_PROFILE)
        .and_then(|value| value.get("email"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("OpenAI account")
        .to_owned();
    Ok(NewCredential {
        provider: "openai-codex".into(),
        kind: CredentialKind::OAuth {
            access,
            refresh,
            expires_ms,
            account_id: Some(auth.clone()),
            email: Some(email.clone()),
        },
        identity_key: format!("account:{auth}"),
        label: email,
    })
}
fn jwt_payload(token: &str) -> Result<serde_json::Value, LlmError> {
    let payload = token
        .split('.')
        .nth(1)
        .ok_or_else(|| LlmError::Decode("invalid Codex JWT".into()))?;
    let bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE.decode(payload))
        .map_err(|error| LlmError::Decode(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| LlmError::Decode(error.to_string()))
}
