//! PKCE challenge generation and paste parsing shared by provider logins.
//! The parser accepts both Anthropic's `code#state` clipboard value and a full
//! redirect URL so a headless coordinator needs no local callback listener.

use crate::error::LlmError;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

pub(crate) fn generate_pkce() -> Result<(String, String), LlmError> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| LlmError::Network(error.to_string()))?;
    let verifier = URL_SAFE_NO_PAD.encode(bytes);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok((verifier, challenge))
}

pub fn parse_authorization_input(input: &str) -> Result<(String, Option<String>), LlmError> {
    let value = input.trim();
    if value.is_empty() {
        return Err(LlmError::Auth("missing authorization code".into()));
    }
    if let Ok(url) = url::Url::parse(value) {
        let mut code = None;
        let mut state = None;
        for (key, value) in url.query_pairs() {
            if key == "code" {
                code = Some(value.into_owned());
            } else if key == "state" {
                state = Some(value.into_owned());
            }
        }
        return code
            .map(|code| (code, state))
            .ok_or_else(|| LlmError::Auth("redirect URL is missing code".into()));
    }
    if let Some((code, state)) = value.split_once('#') {
        return Ok((code.to_owned(), Some(state.to_owned())));
    }
    if value.contains("code=") {
        let query = value.trim_start_matches('?');
        for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
            if key == "code" {
                let code = value.into_owned();
                let state = url::form_urlencoded::parse(query.as_bytes())
                    .find(|(key, _)| key == "state")
                    .map(|(_, value)| value.into_owned());
                return Ok((code, state));
            }
        }
    }
    Ok((value.to_owned(), None))
}
