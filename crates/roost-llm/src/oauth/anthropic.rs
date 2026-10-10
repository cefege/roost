//! Anthropic Claude Pro/Max copy-code OAuth, matching pi-ai's PKCE constants.
//! Login exchanges the pasted code and resolves the account email and org UUID
//! from Anthropic's OAuth profile before storing the credential.

use super::{
    LoginSession, NewCredential, add_notice, add_prompt, generate_pkce, parse_authorization_input,
    token_fields,
};
use crate::{credentials::CredentialKind, error::LlmError};

const CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const AUTHORIZE_URL: &str = "https://claude.ai/oauth/authorize";
const CALLBACK_URL: &str = "https://platform.claude.com/oauth/code/callback";
const SCOPES: &str = "org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";

pub(super) fn start(session: &mut LoginSession) -> Result<(), LlmError> {
    let (verifier, challenge) = generate_pkce()?;
    let state = verifier.clone();
    let mut url =
        url::Url::parse(AUTHORIZE_URL).map_err(|error| LlmError::Auth(error.to_string()))?;
    url.query_pairs_mut()
        .append_pair("code", "true")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", CALLBACK_URL)
        .append_pair("scope", SCOPES)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state);
    session.verifier = Some(verifier);
    session.state_token = Some(state);
    add_notice(
        session,
        "auth_url",
        "Complete login in your browser, then paste the code Anthropic shows.",
        Some(url.to_string()),
        None,
    );
    add_prompt(
        session,
        "manual_code",
        "manual_code",
        "Paste the authorization code or redirect URL:",
    );
    Ok(())
}

pub(super) async fn respond(
    session: &mut LoginSession,
    input: &str,
) -> Result<NewCredential, LlmError> {
    let (code, state) = parse_authorization_input(input)?;
    let verifier = session
        .verifier
        .as_deref()
        .ok_or_else(|| LlmError::Auth("PKCE verifier missing".into()))?;
    if state.as_deref().is_some_and(|state| state != verifier) {
        return Err(LlmError::Auth("OAuth state mismatch".into()));
    }
    let base = session
        .endpoints
        .base("anthropic-console", "https://platform.claude.com");
    let response = session.http.post(format!("{base}/v1/oauth/token")).json(&serde_json::json!({"grant_type":"authorization_code","client_id":CLIENT_ID,"code":code,"state":state.unwrap_or_else(|| verifier.into()),"redirect_uri":CALLBACK_URL,"code_verifier":verifier})).send().await?;
    let status = response.status();
    let data: serde_json::Value = response
        .json()
        .await
        .map_err(|error| LlmError::Decode(error.to_string()))?;
    if !status.is_success() {
        return Err(LlmError::Auth(format!(
            "Anthropic token exchange HTTP {status}: {data}"
        )));
    }
    let (access, refresh, expires_ms) = token_fields(&data)?;
    let profile_base = session
        .endpoints
        .base("anthropic-console", "https://api.anthropic.com");
    let profile = session
        .http
        .get(format!("{profile_base}/api/oauth/profile"))
        .bearer_auth(&access)
        .header("anthropic-beta", "oauth-2025-04-20")
        .send()
        .await?;
    let profile_status = profile.status();
    let profile: serde_json::Value = profile
        .json()
        .await
        .map_err(|error| LlmError::Decode(error.to_string()))?;
    if !profile_status.is_success() {
        return Err(LlmError::Auth(format!(
            "Anthropic profile HTTP {profile_status}: {profile}"
        )));
    }
    let email = find_string(&profile, &["email", "user_email"])
        .ok_or_else(|| LlmError::Decode("Anthropic profile missing email".into()))?;
    let org = find_string(
        &profile,
        &["org_uuid", "organization_uuid", "organization_id"],
    )
    .or_else(|| {
        profile
            .get("organization")
            .and_then(|organization| organization.get("uuid"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    })
    .ok_or_else(|| LlmError::Decode("Anthropic profile missing organization id".into()))?;
    Ok(NewCredential {
        provider: "anthropic".into(),
        kind: CredentialKind::OAuth {
            access,
            refresh,
            expires_ms,
            account_id: None,
            email: Some(email.clone()),
        },
        identity_key: format!("email:{email}|org:{org}"),
        label: email,
    })
}
fn find_string(value: &serde_json::Value, names: &[&str]) -> Option<String> {
    let mut objects = vec![value];
    if let Some(account) = value.get("account") {
        objects.push(account);
    }
    if let Some(user) = value.get("user") {
        objects.push(user);
    }
    objects.into_iter().find_map(|object| {
        names.iter().find_map(|name| {
            object
                .get(*name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
    })
}
