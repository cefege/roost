//! Configured API-key authentication and inherited login methods.
//! Used by models.json provider composition, without application state.
//! Extracted from revpidev/rpi 7ee970aad1d825b30b9413ce251c98e610214e2b.

use std::sync::Arc;
use crate::auth::{ApiKeyAuth, ApiKeyCredential, AuthCheck, AuthContext, AuthInteraction,
    AuthResult, AuthType, ModelAuth, ModelsError, ModelsErrorCode, ProviderAuth};
use crate::auth::interaction::AuthPrompt;
use crate::auth::config_value::{get_config_value_env_var_names, is_command_config_value,
    resolve_config_value_or_throw};
use crate::types::{ProviderEnv, ProviderHeaders};
use crate::utils::headers::merge_headers;

struct ConfiguredApiKeyAuth {
    inner: Arc<dyn ApiKeyAuth>,
    headers: Option<ProviderHeaders>,
    auth_header: bool,
}

#[async_trait::async_trait]
impl ApiKeyAuth for ConfiguredApiKeyAuth {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn has_check(&self) -> bool {
        self.inner.has_check()
    }

    fn supports_login(&self) -> bool {
        self.inner.supports_login()
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, ModelsError> {
        self.inner.login(interaction).await
    }

    async fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthCheck>, ModelsError> {
        self.inner.check(ctx, credential).await
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(mut result) = self.inner.resolve(ctx, credential).await? else {
            return Ok(None);
        };
        result.auth.headers = merge_headers(result.auth.headers.as_ref(), self.headers.as_ref());
        if self.auth_header {
            let Some(api_key) = result.auth.api_key.clone() else {
                return Err(ModelsError::new(
                    ModelsErrorCode::Auth,
                    "authHeader requires a resolved API key",
                ));
            };
            let authorization: ProviderHeaders = [(
                "Authorization".to_owned(),
                Some(format!("Bearer {api_key}")),
            )]
            .into_iter()
            .collect();
            result.auth.headers = merge_headers(result.auth.headers.as_ref(), Some(&authorization));
        }
        Ok(Some(result))
    }
}

pub(super) fn wrap_api_key_auth(
    inner: Arc<dyn ApiKeyAuth>,
    headers: &Option<ProviderHeaders>,
    auth_header: bool,
) -> Arc<dyn ApiKeyAuth> {
    if headers.is_none() && !auth_header {
        return inner;
    }
    Arc::new(ConfiguredApiKeyAuth {
        inner,
        headers: headers.clone(),
        auth_header,
    })
}

pub(super) fn wrap_provider_auth(
    mut auth: ProviderAuth,
    headers: &Option<ProviderHeaders>,
    auth_header: bool,
) -> ProviderAuth {
    if let Some(api_key) = auth.api_key.take() {
        auth.api_key = Some(wrap_api_key_auth(api_key, headers, auth_header));
    }
    auth
}

async fn config_context_env(values: &[String], ctx: &dyn AuthContext) -> Option<ProviderEnv> {
    let mut env = ProviderEnv::new();
    for value in values {
        for name in get_config_value_env_var_names(value) {
            if env.contains_key(&name) {
                continue;
            }
            if let Some(resolved) = ctx.env(&name).await {
                env.insert(name, resolved);
            }
        }
    }
    (!env.is_empty()).then_some(env)
}

pub(super) struct ConfigApiKeyAuth {
    name: String,
    provider_id: String,
    raw_key: String,
    inherited: Option<Arc<dyn ApiKeyAuth>>,
}

impl ConfigApiKeyAuth {
    pub(super) fn new(provider_id: &str, raw_key: String, inherited: Option<Arc<dyn ApiKeyAuth>>) -> Self {
        let name = inherited
            .as_ref()
            .map(|auth| auth.name().to_owned())
            .unwrap_or_else(|| "API key".to_owned());
        Self {
            name,
            provider_id: provider_id.to_owned(),
            raw_key,
            inherited,
        }
    }
}

#[async_trait::async_trait]
impl ApiKeyAuth for ConfigApiKeyAuth {
    fn name(&self) -> &str {
        &self.name
    }

    fn has_check(&self) -> bool {
        true
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, ModelsError> {
        if let Some(inherited) = &self.inherited
            && inherited.supports_login()
        {
            return inherited.login(interaction).await;
        }
        let key = interaction
            .prompt(AuthPrompt::secret("Enter API key".to_owned()))
            .await?;
        Ok(ApiKeyCredential {
            key: Some(key),
            env: None,
        })
    }

    async fn check(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthCheck>, ModelsError> {
        if let Some(credential) = credential {
            if let Some(inherited) = &self.inherited {
                return inherited.check(ctx, Some(credential)).await;
            }
            if credential.key.as_deref().is_some_and(|key| !key.is_empty()) {
                return Ok(Some(AuthCheck {
                    source: Some("stored credential".to_owned()),
                    kind: AuthType::ApiKey,
                }));
            }
            return Ok(None);
        }
        if is_command_config_value(&self.raw_key) {
            return Ok(Some(AuthCheck {
                source: Some("configured API key".to_owned()),
                kind: AuthType::ApiKey,
            }));
        }
        for name in get_config_value_env_var_names(&self.raw_key) {
            if ctx.env(&name).await.is_none() {
                return Ok(None);
            }
        }
        Ok(Some(AuthCheck {
            source: Some("configured API key".to_owned()),
            kind: AuthType::ApiKey,
        }))
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        if let Some(credential) = credential {
            if let Some(inherited) = &self.inherited {
                return inherited.resolve(ctx, Some(credential)).await;
            }
            if let Some(key) = credential.key.clone().filter(|key| !key.is_empty()) {
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
            return Ok(None);
        }
        let env = config_context_env(std::slice::from_ref(&self.raw_key), ctx).await;
        let key = resolve_config_value_or_throw(
            &self.raw_key,
            &format!("API key for provider \"{}\"", self.provider_id),
            env.as_ref(),
        )?;
        if let Some(inherited) = &self.inherited {
            return inherited
                .resolve(
                    ctx,
                    Some(&ApiKeyCredential {
                        key: Some(key),
                        env: env.clone(),
                    }),
                )
                .await;
        }
        Ok(Some(AuthResult {
            auth: ModelAuth {
                api_key: Some(key),
                headers: None,
                base_url: None,
            },
            env,
            source: Some("configured API key".to_owned()),
        }))
    }
}

