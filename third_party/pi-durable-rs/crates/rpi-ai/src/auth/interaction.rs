//! Port of the `AuthPrompt` / `AuthEvent` / `AuthInteraction` half of
//! `packages/ai/src/auth/types.ts` @ pi 0.82.1 (2efa728).
//!
//! Intentional differences: `AbortSignal` becomes
//! `tokio_util::sync::CancellationToken`; the `signal` property becomes a
//! trait method with a `None` default. `prompt` failures are reported as
//! [`ModelsError`] (upstream rejects with `Error`).

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use super::resolve::ModelsError;
use super::types::BoxFutureSend;

/// One option of a `select` prompt (`{ id, label, description? }`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SelectOption {
    pub id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `AuthPrompt` — prompt shown to the user during login. The per-prompt
/// `signal` lets the flow cancel a pending prompt when an out-of-band event
/// resolves the step, e.g. a `manual_code` prompt raced against a callback
/// server, cancelled when the callback wins.
///
/// `signal` never serializes (a fresh token deserializes as `None`-default
/// through `skip`); it is process-local control state, not wire data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AuthPrompt {
    #[serde(rename = "text")]
    Text {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(skip)]
        signal: Option<CancellationToken>,
    },
    #[serde(rename = "secret")]
    Secret {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(skip)]
        signal: Option<CancellationToken>,
    },
    #[serde(rename = "select")]
    Select {
        message: String,
        options: Vec<SelectOption>,
        #[serde(skip)]
        signal: Option<CancellationToken>,
    },
    #[serde(rename = "manual_code")]
    ManualCode {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        placeholder: Option<String>,
        #[serde(skip)]
        signal: Option<CancellationToken>,
    },
}

impl AuthPrompt {
    /// Convenience constructor for a `secret` prompt without placeholder or
    /// per-prompt signal (the common login case).
    pub fn secret(message: impl Into<String>) -> Self {
        AuthPrompt::Secret {
            message: message.into(),
            placeholder: None,
            signal: None,
        }
    }

    /// The per-prompt cancellation signal, if any.
    pub fn signal(&self) -> Option<CancellationToken> {
        match self {
            AuthPrompt::Text { signal, .. }
            | AuthPrompt::Secret { signal, .. }
            | AuthPrompt::Select { signal, .. }
            | AuthPrompt::ManualCode { signal, .. } => signal.clone(),
        }
    }
}

/// `AuthInfoLink`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthInfoLink {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `AuthEvent` — progress events surfaced by login flows. Field names
/// serialize camelCase (`userCode`, `verificationUri`, …) per coding-standards
/// §4.4. Messages/URLs are operator-facing, never secrets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all_fields = "camelCase")]
pub enum AuthEvent {
    #[serde(rename = "info")]
    Info {
        message: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        links: Option<Vec<AuthInfoLink>>,
    },
    #[serde(rename = "auth_url")]
    AuthUrl {
        url: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    #[serde(rename = "device_code")]
    DeviceCode {
        user_code: String,
        verification_uri: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        interval_seconds: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expires_in_seconds: Option<u64>,
    },
    #[serde(rename = "progress")]
    Progress { message: String },
}

/// `AuthInteraction` — login interaction callbacks serving both api-key and
/// OAuth flows.
///
/// `prompt()` returns the entered/selected string (`select` returns the
/// option id) and errs on cancel/abort. `signal()` aborts the whole login
/// flow; per-prompt cancellation uses [`AuthPrompt`]'s per-prompt signal.
pub trait AuthInteraction: Send + Sync {
    /// `signal?: AbortSignal` — whole-flow cancellation. Absent by default.
    fn signal(&self) -> Option<CancellationToken> {
        None
    }

    /// `prompt(prompt: AuthPrompt): Promise<string>`.
    fn prompt<'a>(&'a self, prompt: AuthPrompt) -> BoxFutureSend<'a, Result<String, ModelsError>>;

    /// `notify(event: AuthEvent): void`.
    fn notify(&self, event: AuthEvent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_event_serializes_camel_case() {
        let event = AuthEvent::DeviceCode {
            user_code: "ABCD-EFGH".to_owned(),
            verification_uri: "https://example.com/device".to_owned(),
            interval_seconds: Some(5),
            expires_in_seconds: None,
        };
        let value = serde_json::to_value(&event).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({
                "type": "device_code",
                "userCode": "ABCD-EFGH",
                "verificationUri": "https://example.com/device",
                "intervalSeconds": 5
            })
        );
    }

    #[test]
    fn auth_prompt_signal_is_not_serialized() {
        let prompt = AuthPrompt::Secret {
            message: "Enter key".to_owned(),
            placeholder: None,
            signal: Some(CancellationToken::new()),
        };
        let value = serde_json::to_value(&prompt).expect("serialize");
        assert_eq!(
            value,
            serde_json::json!({ "type": "secret", "message": "Enter key" })
        );
    }
}
