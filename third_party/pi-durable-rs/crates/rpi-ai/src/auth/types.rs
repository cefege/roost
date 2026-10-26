//! Port of `packages/ai/src/auth/types.ts` @ pi 0.82.1 (2efa728) — T03
//! skeleton: credential types, store/auth interfaces. T04 added
//! `login`/`check` to the auth traits; `AuthPrompt`/`AuthEvent`/
//! `AuthInteraction` live in `super::interaction` (mirroring upstream, where
//! all of these share `auth/types.ts`). OAuth flows land with T04 part 2.
//!
//! Intentional differences: TS method interfaces become `async_trait` traits;
//! the `signal` becomes `CancellationToken`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

use super::interaction::AuthInteraction;
use super::resolve::{ModelsError, ModelsErrorCode};
use crate::types::{ProviderEnv, ProviderHeaders};

/// `ModelAuth` — request auth for a single model request.
#[derive(Clone, Default, PartialEq)]
pub struct ModelAuth {
    pub api_key: Option<String>,
    pub headers: Option<ProviderHeaders>,
    pub base_url: Option<String>,
}

// Redacted Debug (coding-standards §11.1): `api_key` and header values (they
// can carry `Authorization`) never appear in output; structure and
// non-sensitive fields stay visible.
impl fmt::Debug for ModelAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelAuth")
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field(
                "headers",
                &self
                    .headers
                    .as_ref()
                    .map(|headers| headers.keys().collect::<Vec<_>>()),
            )
            .field("base_url", &self.base_url)
            .finish()
    }
}

/// `ApiKeyCredential` — stored api-key credential. `env` holds provider-scoped
/// environment/config values such as Cloudflare account/gateway ids.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ApiKeyCredential {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<ProviderEnv>,
}

// Redacted Debug (coding-standards §11.1): the key never appears in output.
impl fmt::Debug for ApiKeyCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiKeyCredential")
            .field("key", &self.key.as_ref().map(|_| "[redacted]"))
            .field("env", &self.env)
            .finish()
    }
}

/// `OAuthCredential` — stored canonical OAuth credential. Extension-provided
/// extra fields are preserved via the flattened `extra` map (`[key: string]:
/// unknown` upstream).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthCredential {
    pub refresh: String,
    pub access: String,
    pub expires: i64,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// Redacted Debug (coding-standards §11.1): `refresh`/`access` never appear in
// output; `extra` values are extension-controlled and may hold tokens, so
// only the keys are shown.
impl fmt::Debug for OAuthCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthCredential")
            .field("refresh", &"[redacted]")
            .field("access", &"[redacted]")
            .field("expires", &self.expires)
            .field("extra", &self.extra.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// `Credential` — one type-tagged credential per provider (auth.json shape).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Credential {
    #[serde(rename = "api_key")]
    ApiKey(ApiKeyCredential),
    #[serde(rename = "oauth")]
    OAuth(OAuthCredential),
}

// Redacted via the inner types' Debug impls (coding-standards §11.1).
impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Credential::ApiKey(credential) => f.debug_tuple("ApiKey").field(credential).finish(),
            Credential::OAuth(credential) => f.debug_tuple("OAuth").field(credential).finish(),
        }
    }
}

impl Credential {
    pub fn credential_type(&self) -> CredentialType {
        match self {
            Credential::ApiKey(_) => CredentialType::ApiKey,
            Credential::OAuth(_) => CredentialType::Oauth,
        }
    }
}

/// `Credential["type"]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CredentialType {
    #[serde(rename = "api_key")]
    ApiKey,
    #[serde(rename = "oauth")]
    Oauth,
}

/// `CredentialInfo` — non-secret credential metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialInfo {
    pub provider_id: String,
    #[serde(rename = "type")]
    pub credential_type: CredentialType,
}

/// Boxed future used by the store/auth traits.
pub type BoxFutureSend<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Callback for [`CredentialStore::modify`]: sees the current credential,
/// returns the new one (`None` leaves the entry unchanged).
pub type ModifyFn = Arc<
    dyn Fn(Option<Credential>) -> BoxFutureSend<'static, Result<Option<Credential>, ModelsError>>
        + Send
        + Sync,
>;

/// `AuthOperationOptions` (auth/types.ts:45-48 @ 4181f66) — optional
/// cancellation for public auth and credential operations. The upstream
/// `AbortSignal` becomes a `CancellationToken` (project convention).
#[derive(Debug, Clone, Default)]
pub struct AuthOperationOptions {
    pub signal: Option<CancellationToken>,
}

impl AuthOperationOptions {
    pub fn with_signal(signal: CancellationToken) -> Self {
        Self {
            signal: Some(signal),
        }
    }

    /// `signal.throwIfAborted()` equivalent.
    pub(crate) fn throw_if_cancelled(options: Option<&Self>) -> Result<(), ModelsError> {
        if options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ModelsError::aborted());
        }
        Ok(())
    }
}

/// `CredentialStore` — app-owned credential storage keyed by provider id.
/// `modify` is the only write path (serialized read-modify-write). All
/// operations accept optional cancellation (auth/types.ts:65-94 @ 4181f66):
/// a cancelled `modify`/`delete` queued behind another mutation rejects
/// without ever running its task.
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    /// Read the stored credential, possibly expired. `None` for missing.
    async fn read(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<Credential>, ModelsError>;

    /// List stored credential metadata without resolving or exposing secrets.
    async fn list(
        &self,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Vec<CredentialInfo>, ModelsError>;

    /// Serialized write — the only write path. Resolves with the post-write
    /// credential (current when the callback returns `None`).
    async fn modify(
        &self,
        provider_id: &str,
        f: ModifyFn,
        options: Option<&AuthOperationOptions>,
    ) -> Result<Option<Credential>, ModelsError>;

    /// Remove a credential (logout). Serialized against `modify`.
    async fn delete(
        &self,
        provider_id: &str,
        options: Option<&AuthOperationOptions>,
    ) -> Result<(), ModelsError>;
}

/// `AuthContext` — environment access for auth resolution (injectable).
#[async_trait::async_trait]
pub trait AuthContext: Send + Sync {
    async fn env(&self, name: &str) -> Option<String>;
    /// Check whether a file exists. Supports a leading `~`.
    async fn file_exists(&self, path: &str) -> bool;
}

/// `defaultAuthContext` (auth/context.ts): process env + filesystem.
pub struct DefaultAuthContext;

#[async_trait::async_trait]
impl AuthContext for DefaultAuthContext {
    async fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }

    async fn file_exists(&self, path: &str) -> bool {
        let expanded = if let Some(rest) = path.strip_prefix("~/") {
            std::env::var("HOME")
                .map(|home| format!("{home}/{rest}"))
                .unwrap_or_else(|_| path.to_owned())
        } else {
            path.to_owned()
        };
        tokio::fs::try_exists(expanded).await.unwrap_or(false)
    }
}

/// `AuthResult` — result of resolving auth for a model.
#[derive(Clone, Default, PartialEq)]
pub struct AuthResult {
    pub auth: ModelAuth,
    /// Provider-scoped environment/config values resolved from credentials
    /// and ambient context.
    pub env: Option<ProviderEnv>,
    /// Human-readable label for status UI: "ANTHROPIC_API_KEY", "OAuth",
    /// "~/.aws/credentials".
    pub source: Option<String>,
}

// Redacted via `ModelAuth`'s Debug impl (coding-standards §11.1).
impl fmt::Debug for AuthResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthResult")
            .field("auth", &self.auth)
            .field("env", &self.env)
            .field("source", &self.source)
            .finish()
    }
}

/// `AuthType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    ApiKey,
    Oauth,
}

/// `AuthCheck`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthCheck {
    pub source: Option<String>,
    pub kind: AuthType,
}

/// `ApiKeyAuth` — api-key auth: stored key/provider env plus ambient sources
/// (env vars, AWS profiles, ADC files). Ambient-only providers omit `login`.
#[async_trait::async_trait]
pub trait ApiKeyAuth: Send + Sync {
    /// Display name, e.g. "Anthropic API key".
    fn name(&self) -> &str;

    /// `login?` presence (upstream: the method is absent for ambient-only
    /// providers, models.ts:435 `method?.login`, interactive-mode.ts:4928).
    /// Defaults to `false`; implementations that override [`Self::login`]
    /// must return `true` so the interactive layer can pick the login dialog
    /// over the ambient-info dialog.
    fn supports_login(&self) -> bool {
        false
    }

    /// `login?` — interactive setup (prompt for key/provider env). The
    /// default errors: ambient-only providers have no login (upstream: the
    /// method is absent).
    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, ModelsError> {
        Err(ModelsError::new(
            ModelsErrorCode::Auth,
            format!(
                "{} is ambient-only: interactive login is not supported",
                self.name()
            ),
        ))
    }

    /// `check?` presence (upstream: the method is absent vs. present is
    /// behaviorally distinct — models.ts:497-508 calls `check` and RETURNS its
    /// result when the method exists; it only falls through to `resolve` when
    /// the method is ABSENT). The Rust trait cannot express "method absent",
    /// so this flag marks implementations whose `Ok(None)` means "checked:
    /// not configured" — callers must not fall through to `resolve()` (which
    /// may throw where `check` returns None, e.g. a missing `${VAR}` config
    /// value). Implementations that override [`Self::check`] with real logic
    /// must return `true`.
    fn has_check(&self) -> bool {
        false
    }

    /// `check?` — optional side-effect-free availability check. Use this when
    /// `resolve()` may execute commands or perform other request-time work.
    /// Default `None`: Models checks availability by resolving auth.
    async fn check(
        &self,
        _ctx: &dyn AuthContext,
        _credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthCheck>, ModelsError> {
        Ok(None)
    }

    /// Resolve auth from the stored credential and/or ambient sources, merging
    /// per field. `None` = not configured.
    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError>;
}

/// `LoginOptions` (auth/types.ts:202-208 @ abe508e1b) — app-supplied
/// context for login flows.
#[derive(Clone, Default)]
pub struct LoginOptions {
    /// Returns the stable ID of this app installation, e.g. sent to OpenAI as
    /// its agent host ID. Called only by login flows that need it, so apps
    /// can create the ID on first use and must return the same ID on every
    /// later call.
    pub get_device_id: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// Name this app introduces itself with during login, e.g. OpenAI's agent
    /// name hint and the Codex originator (auth/types.ts:206-211 @ `9ad083102`).
    /// Defaults to rpi's own name.
    pub agent_name: Option<String>,
}

// Manual Debug: the callback is not a secret, but it has no Debug impl.
impl fmt::Debug for LoginOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginOptions")
            .field("get_device_id", &self.get_device_id.is_some())
            .field("agent_name", &self.agent_name)
            .finish()
    }
}

/// `OAuthAuth` — the `refresh`/`toAuth` split lets `Models` own the locked
/// refresh pattern.
#[async_trait::async_trait]
pub trait OAuthAuth: Send + Sync {
    /// Display name, e.g. "Anthropic (Claude Pro/Max)".
    fn name(&self) -> &str;

    /// `isSubscription?` (packages/ai/src/auth/types.ts:210-211 @ 4181f66,
    /// v0.84): whether access through this auth method is backed by a
    /// provider subscription. Metadata only — drives the footer "(sub)"
    /// badge upstream; no auth-flow behavior reads it. Default `false`.
    fn is_subscription(&self) -> bool {
        false
    }

    /// `login` — interactive OAuth flow (PKCE / device code / localhost
    /// callback). `options` carries app-supplied context such as the stable
    /// installation ID. Required upstream; the default here errors so partial
    /// implementations stay constructible until the flows are wired.
    async fn login(
        &self,
        _interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        Err(ModelsError::new(
            ModelsErrorCode::Auth,
            format!("{}: OAuth login is not supported", self.name()),
        ))
    }

    /// Exchange the refresh token. Network call; errors on failure.
    /// `Models` runs this under the store lock.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError>;

    /// Side-effect-free derivation of request auth from a valid credential.
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError>;
}

/// `ProviderAuth`. At least one of `api_key`/`oauth` must be present.
#[derive(Clone, Default)]
pub struct ProviderAuth {
    pub api_key: Option<Arc<dyn ApiKeyAuth>>,
    pub oauth: Option<Arc<dyn OAuthAuth>>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Redaction (coding standards §11.1/§11.2): credential secret values must
    /// not appear in `{:?}` output; structure and non-sensitive fields stay visible.
    #[test]
    fn debug_output_redacts_credential_secrets() {
        let mut headers = ProviderHeaders::new();
        headers.insert(
            "Authorization".to_owned(),
            Some("Bearer super-secret-token".to_owned()),
        );
        let model_auth = ModelAuth {
            api_key: Some("sk-secret-key".to_owned()),
            headers: Some(headers),
            base_url: Some("https://api.example.com".to_owned()),
        };
        let debug = format!("{model_auth:?}");
        assert!(!debug.contains("sk-secret-key"));
        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("[redacted]"));
        assert!(debug.contains("Authorization"));
        assert!(debug.contains("https://api.example.com"));

        let api_key = ApiKeyCredential {
            key: Some("sk-secret-key".to_owned()),
            env: Some(ProviderEnv::from([(
                "ACCOUNT_ID".to_owned(),
                "acct-1".to_owned(),
            )])),
        };
        let debug = format!("{api_key:?}");
        assert!(!debug.contains("sk-secret-key"));
        assert!(debug.contains("acct-1"));

        let mut extra = Map::new();
        extra.insert("accountId".to_owned(), json!("acct-secret-ish"));
        let oauth = OAuthCredential {
            refresh: "refresh-secret".to_owned(),
            access: "access-secret".to_owned(),
            expires: 123,
            extra,
        };
        let credential = Credential::OAuth(oauth);
        let debug = format!("{credential:?}");
        assert!(!debug.contains("refresh-secret"));
        assert!(!debug.contains("access-secret"));
        assert!(!debug.contains("acct-secret-ish"));
        assert!(debug.contains("expires: 123"));
        assert!(debug.contains("accountId"));

        let result = AuthResult {
            auth: model_auth,
            env: None,
            source: Some("ANTHROPIC_API_KEY".to_owned()),
        };
        let debug = format!("{result:?}");
        assert!(!debug.contains("sk-secret-key"));
        assert!(!debug.contains("super-secret-token"));
        assert!(debug.contains("ANTHROPIC_API_KEY"));

        // api_key entries are redacted the same way.
        let credential = Credential::ApiKey(api_key);
        assert!(!format!("{credential:?}").contains("sk-secret-key"));
    }
}
