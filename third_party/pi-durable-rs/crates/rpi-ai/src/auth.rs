//! Port of `packages/ai/src/auth/` @ pi 0.82.1 (2efa728).
//!
//! Auth interfaces (credential types, store, resolution) plus the T04 part 1
//! building blocks: the key-value config DSL (`config_value`, port of
//! `coding-agent/src/core/resolve-config-value.ts`), the auth.json-backed
//! credential store (`file_store`, port of
//! `coding-agent/src/core/auth-storage.ts`), the known env-var table
//! (`env_keys`, port of `env-api-keys.ts`), standard provider auth helpers
//! (`helpers` / `anthropic_auth`) and the login interaction surface
//! (`interaction`). OAuth flows land with T04 part 2.

pub mod anthropic_auth;
pub mod anthropic_federation;
pub mod cloudflare_auth;
pub mod config_value;
pub mod credential_store;
pub mod env_keys;
pub mod file_store;
pub mod helpers;
pub mod interaction;
pub mod oauth;
pub mod resolve;
pub mod types;

pub use anthropic_auth::{AnthropicApiKeyAuth, anthropic_api_key_auth};
pub use anthropic_federation::{
    AnthropicFederationConfig, get_access_token, get_anthropic_federation, invalidate_access_token,
};
pub use credential_store::InMemoryCredentialStore;
pub use env_keys::{
    ANTHROPIC_API_KEY_ENV, ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_FEDERATION_RULE_ID_ENV,
    ANTHROPIC_IDENTITY_TOKEN_FILE_ENV, ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_ORGANIZATION_ID_ENV,
    ANTHROPIC_SERVICE_ACCOUNT_ID_ENV, ANTHROPIC_WORKSPACE_ID_ENV, api_key_env_vars, find_env_keys,
    get_env_api_key,
};
pub use file_store::{
    Backend, FileAuthStorageBackend, FileCredentialStore, InMemoryAuthStorageBackend,
    read_stored_credential,
};
pub use helpers::{EnvApiKeyAuth, env_api_key_auth};
pub use interaction::{AuthEvent, AuthInfoLink, AuthInteraction, AuthPrompt, SelectOption};
pub use oauth::{OAuthCallbackServer, anthropic_oauth};
pub use resolve::{AuthResolutionOverrides, ModelsError, ModelsErrorCode, resolve_provider_auth};
pub use types::*;
