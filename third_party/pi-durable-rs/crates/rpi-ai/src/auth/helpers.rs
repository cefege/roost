//! Port of `packages/ai/src/auth/helpers.ts` @ pi 0.82.1 (2efa728) —
//! `envApiKeyAuth` only (`lazyOAuth` exists to keep Node-only flow code out
//! of browser bundles; rpi is a native binary and has no bundle-splitting
//! constraint, so it is not ported). Because there is no `lazyOAuth` wrapper,
//! the wrapper-level `isSubscription` flag (auth/helpers.ts:40-59 @ 4181f66)
//! lives directly on the five `OAuthAuth` impls instead
//! (`OAuthAuth::is_subscription`).

use super::interaction::{AuthInteraction, AuthPrompt};
use super::resolve::ModelsError;
use super::types::{ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult, ModelAuth};

/// `envApiKeyAuth` — standard api-key auth: a stored credential key wins,
/// otherwise the first set env var resolves. Includes a `login` that prompts
/// for the key. Providers with non-standard resolution (provider env,
/// ambient files, IAM) write their own [`ApiKeyAuth`].
pub struct EnvApiKeyAuth {
    name: String,
    env_vars: Vec<String>,
}

/// `envApiKeyAuth(name, envVars)`.
pub fn env_api_key_auth(name: impl Into<String>, env_vars: &[&str]) -> EnvApiKeyAuth {
    EnvApiKeyAuth {
        name: name.into(),
        env_vars: env_vars
            .iter()
            .map(|env_var| (*env_var).to_owned())
            .collect(),
    }
}

#[async_trait::async_trait]
impl ApiKeyAuth for EnvApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, ModelsError> {
        let key = interaction
            .prompt(AuthPrompt::secret(format!("Enter {}", self.name)))
            .await?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
        })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        if let Some(credential) = credential
            && let Some(key) = credential.key.clone().filter(|key| !key.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key),
                    headers: None,
                    base_url: None,
                },
                env: credential.env.clone(),
                source: Some("stored credential".to_owned()),
            }));
        }
        for env_var in &self.env_vars {
            // JS `if (value)` — empty strings are falsy.
            if let Some(value) = ctx.env(env_var).await.filter(|value| !value.is_empty()) {
                return Ok(Some(AuthResult {
                    auth: ModelAuth {
                        api_key: Some(value),
                        headers: None,
                        base_url: None,
                    },
                    env: None,
                    source: Some(env_var.clone()),
                }));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    //! No dedicated upstream test file for `helpers.ts`; these pin the
    //! upstream resolution order and login prompt shape.

    use std::collections::HashMap;
    use std::sync::Mutex;

    use super::*;
    use crate::auth::interaction::AuthEvent;
    use crate::auth::types::BoxFutureSend;

    struct MapAuthContext(HashMap<String, String>);

    #[async_trait::async_trait]
    impl AuthContext for MapAuthContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }

        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    struct RecordingInteraction {
        prompts: Mutex<Vec<AuthPrompt>>,
        answer: String,
    }

    impl AuthInteraction for RecordingInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            Box::pin(async move {
                self.prompts
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(prompt);
                Ok(self.answer.clone())
            })
        }

        fn notify(&self, _event: AuthEvent) {}
    }

    #[tokio::test]
    async fn stored_key_wins_over_env_vars() {
        let auth = env_api_key_auth("Test API key", &["TEST_HELPERS_ENV_KEY"]);
        let ctx = MapAuthContext(HashMap::from([(
            "TEST_HELPERS_ENV_KEY".to_owned(),
            "env-key".to_owned(),
        )]));
        let credential = ApiKeyCredential {
            key: Some("stored-key".to_owned()),
            env: None,
        };
        let result = auth
            .resolve(&ctx, Some(&credential))
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
        assert_eq!(result.source.as_deref(), Some("stored credential"));
    }

    #[tokio::test]
    async fn first_set_env_var_resolves_in_order() {
        let auth = env_api_key_auth(
            "Test API key",
            &["TEST_HELPERS_ENV_A", "TEST_HELPERS_ENV_B"],
        );
        let ctx = MapAuthContext(HashMap::from([(
            "TEST_HELPERS_ENV_B".to_owned(),
            "b-key".to_owned(),
        )]));
        let result = auth
            .resolve(&ctx, None)
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("b-key"));
        assert_eq!(result.source.as_deref(), Some("TEST_HELPERS_ENV_B"));

        let ctx = MapAuthContext(HashMap::new());
        assert!(auth.resolve(&ctx, None).await.expect("resolve").is_none());
    }

    #[tokio::test]
    async fn login_prompts_for_the_key_with_a_secret_prompt() {
        let auth = env_api_key_auth("Test API key", &[]);
        let interaction = RecordingInteraction {
            prompts: Mutex::new(Vec::new()),
            answer: "entered-key".to_owned(),
        };
        let credential = auth.login(&interaction).await.expect("login");
        assert_eq!(credential.key.as_deref(), Some("entered-key"));
        let prompts = interaction
            .prompts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(prompts.len(), 1);
        match &prompts[0] {
            AuthPrompt::Secret { message, .. } => assert_eq!(message, "Enter Test API key"),
            other => panic!("expected secret prompt, got {other:?}"),
        }
    }
}
