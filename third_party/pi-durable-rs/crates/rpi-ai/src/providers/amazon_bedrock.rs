//! Port of `packages/ai/src/providers/amazon-bedrock.ts` @ pi 0.82.1
//! (2efa728), including the provider-owned `bedrockAuth` (defined inline
//! upstream, kept inline here). No `baseUrl` upstream — the bedrock adapter
//! derives the regional endpoint itself.

use std::sync::Arc;

use crate::api::bedrock_converse_stream::BedrockConverseStream;
use crate::auth::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthEvent, AuthInfoLink, AuthInteraction,
    AuthPrompt, AuthResult, ModelAuth, ModelsError, ModelsErrorCode, ProviderAuth, SelectOption,
};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::types::ProviderEnv;

/// `bedrockAuth` — Bedrock accepts a bearer token or the AWS SDK's default
/// credential chain. The login flow can store a token/profile choice;
/// `resolve` also detects ambient AWS credentials without copying them into
/// rpi's credential store.
pub struct BedrockAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for BedrockAuth {
    fn name(&self) -> &str {
        "AWS credentials or bearer token"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, ModelsError> {
        let method = interaction
            .prompt(AuthPrompt::Select {
                message: "Select Amazon Bedrock authentication method:".to_owned(),
                options: vec![
                    SelectOption {
                        id: "bearer-token".to_owned(),
                        label: "Bearer token".to_owned(),
                        description: None,
                    },
                    SelectOption {
                        id: "aws-profile".to_owned(),
                        label: "AWS profile".to_owned(),
                        description: None,
                    },
                    SelectOption {
                        id: "credential-chain".to_owned(),
                        label: "Existing AWS credential chain".to_owned(),
                        description: None,
                    },
                ],
                signal: None,
            })
            .await?;
        if method == "bearer-token" {
            let key = interaction
                .prompt(AuthPrompt::secret("Enter Amazon Bedrock bearer token"))
                .await?;
            return Ok(ApiKeyCredential {
                key: Some(key),
                env: None,
            });
        }
        interaction.notify(AuthEvent::Info {
            message:
                "Amazon Bedrock supports AWS profiles, IAM credentials, and role-based credentials."
                    .to_owned(),
            links: Some(vec![AuthInfoLink {
                url:
                    "https://docs.aws.amazon.com/sdkref/latest/guide/standardized-credentials.html"
                        .to_owned(),
                label: Some("AWS credential provider chain".to_owned()),
            }]),
        });
        if method == "aws-profile" {
            let profile = interaction
                .prompt(AuthPrompt::Text {
                    message: "Enter AWS profile name".to_owned(),
                    placeholder: None,
                    signal: None,
                })
                .await?;
            return Ok(ApiKeyCredential {
                key: None,
                env: Some(ProviderEnv::from([("AWS_PROFILE".to_owned(), profile)])),
            });
        }
        if method != "credential-chain" {
            return Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Unknown Amazon Bedrock auth method: {method}"),
            ));
        }
        interaction
            .prompt(AuthPrompt::Text {
                message: "Configure AWS credentials, then press Enter to continue".to_owned(),
                placeholder: None,
                signal: None,
            })
            .await?;
        Ok(ApiKeyCredential {
            key: None,
            env: None,
        })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        /// `auth: {}` — ambient/chain credentials need no request auth; the
        /// adapter picks them up itself.
        fn ambient_auth(env: Option<ProviderEnv>, source: &str) -> AuthResult {
            AuthResult {
                auth: ModelAuth {
                    api_key: None,
                    headers: None,
                    base_url: None,
                },
                env,
                source: Some(source.to_owned()),
            }
        }

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
        if ctx
            .env("AWS_BEARER_TOKEN_BEDROCK")
            .await
            .is_some_and(|token| !token.is_empty())
        {
            return Ok(Some(ambient_auth(None, "AWS_BEARER_TOKEN_BEDROCK")));
        }
        // `credential?.env?.AWS_PROFILE ?? (await ctx.env("AWS_PROFILE"))`.
        let credential_profile = credential
            .and_then(|credential| credential.env.as_ref())
            .and_then(|env| env.get("AWS_PROFILE"))
            .cloned();
        let profile = match credential_profile.clone() {
            Some(profile) => Some(profile),
            None => ctx.env("AWS_PROFILE").await,
        };
        if profile
            .as_deref()
            .is_some_and(|profile| !profile.is_empty())
        {
            let source = if credential_profile
                .as_deref()
                .is_some_and(|profile| !profile.is_empty())
            {
                "stored credential"
            } else {
                "AWS_PROFILE"
            };
            return Ok(Some(ambient_auth(
                credential.and_then(|credential| credential.env.clone()),
                source,
            )));
        }
        if ctx
            .env("AWS_ACCESS_KEY_ID")
            .await
            .is_some_and(|value| !value.is_empty())
            && ctx
                .env("AWS_SECRET_ACCESS_KEY")
                .await
                .is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(ambient_auth(None, "AWS access keys")));
        }
        if ctx
            .env("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI")
            .await
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(ambient_auth(None, "ECS task role")));
        }
        if ctx
            .env("AWS_CONTAINER_CREDENTIALS_FULL_URI")
            .await
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(ambient_auth(None, "ECS task role")));
        }
        if ctx
            .env("AWS_WEB_IDENTITY_TOKEN_FILE")
            .await
            .is_some_and(|value| !value.is_empty())
        {
            return Ok(Some(ambient_auth(None, "web identity token")));
        }
        Ok(None)
    }
}

/// `amazonBedrockProvider()`.
pub fn amazon_bedrock_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "amazon-bedrock".to_owned(),
        name: Some("Amazon Bedrock".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(BedrockAuth)),
            oauth: None,
        },
        models: get_builtin_models("amazon-bedrock").to_vec(),
        api: ProviderApi::Single(Arc::new(BedrockConverseStream)),
        ..Default::default()
    })
}
