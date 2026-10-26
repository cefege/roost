//! Port of `packages/ai/src/providers/google-vertex.ts` @ pi 0.82.1
//! (2efa728), including the provider-owned `vertexAuth` (defined inline
//! upstream, kept inline here). No `baseUrl` upstream — the vertex adapter
//! builds the endpoint from project/location.

use std::sync::Arc;

use crate::api::google_vertex::GoogleVertex;
use crate::auth::{
    ApiKeyAuth, ApiKeyCredential, AuthContext, AuthEvent, AuthInfoLink, AuthInteraction,
    AuthPrompt, AuthResult, ModelAuth, ModelsError, ModelsErrorCode, ProviderAuth, SelectOption,
};
use crate::generated::get_builtin_models;
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};

/// `VERTEX_ADC_PATH`.
const VERTEX_ADC_PATH: &str = "~/.config/gcloud/application_default_credentials.json";

/// `vertexAuth` — Vertex accepts an explicit API key or Application Default
/// Credentials (`gcloud auth application-default login`). ADC additionally
/// requires project and location env vars, which the adapter reads itself.
pub struct VertexAuth;

#[async_trait::async_trait]
impl ApiKeyAuth for VertexAuth {
    fn name(&self) -> &str {
        "Google Cloud credentials"
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
                message: "Select Google Vertex AI authentication method:".to_owned(),
                options: vec![
                    SelectOption {
                        id: "api-key".to_owned(),
                        label: "Google Cloud API key".to_owned(),
                        description: None,
                    },
                    SelectOption {
                        id: "adc".to_owned(),
                        label: "Application Default Credentials".to_owned(),
                        description: None,
                    },
                    SelectOption {
                        id: "service-account".to_owned(),
                        label: "Service account credentials file".to_owned(),
                        description: None,
                    },
                ],
                signal: None,
            })
            .await?;
        if method == "api-key" {
            let key = interaction
                .prompt(AuthPrompt::secret("Enter Google Cloud API key"))
                .await?;
            return Ok(ApiKeyCredential {
                key: Some(key),
                env: None,
            });
        }
        if method != "adc" && method != "service-account" {
            return Err(ModelsError::new(
                ModelsErrorCode::Auth,
                format!("Unknown Google Vertex AI auth method: {method}"),
            ));
        }
        interaction.notify(AuthEvent::Info {
            message: if method == "adc" {
                "Run `gcloud auth application-default login`, then provide the project and location."
                    .to_owned()
            } else {
                "Provide a service account credentials file, project, and location.".to_owned()
            },
            links: Some(vec![AuthInfoLink {
                url: "https://cloud.google.com/docs/authentication/provide-credentials-adc"
                    .to_owned(),
                label: Some("Application Default Credentials".to_owned()),
            }]),
        });
        let project = interaction
            .prompt(AuthPrompt::Text {
                message: "Enter Google Cloud project ID".to_owned(),
                placeholder: None,
                signal: None,
            })
            .await?;
        let location = interaction
            .prompt(AuthPrompt::Text {
                message: "Enter Google Cloud location".to_owned(),
                placeholder: None,
                signal: None,
            })
            .await?;
        let credentials_path = if method == "service-account" {
            Some(
                interaction
                    .prompt(AuthPrompt::Text {
                        message: "Enter service account credentials file path".to_owned(),
                        placeholder: None,
                        signal: None,
                    })
                    .await?,
            )
        } else {
            None
        };
        let mut env = crate::types::ProviderEnv::from([
            ("GOOGLE_CLOUD_PROJECT".to_owned(), project),
            ("GOOGLE_CLOUD_LOCATION".to_owned(), location),
        ]);
        if let Some(credentials_path) = credentials_path {
            env.insert(
                "GOOGLE_APPLICATION_CREDENTIALS".to_owned(),
                credentials_path,
            );
        }
        Ok(ApiKeyCredential {
            key: None,
            env: Some(env),
        })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        // `credential?.key ?? (await ctx.env("GOOGLE_CLOUD_API_KEY"))`,
        // truthiness-gated (JS `if (key)`).
        let credential_key = credential
            .and_then(|credential| credential.key.clone())
            .filter(|key| !key.is_empty());
        let key = match credential_key.clone() {
            Some(key) => Some(key),
            None => ctx
                .env("GOOGLE_CLOUD_API_KEY")
                .await
                .filter(|key| !key.is_empty()),
        };
        if let Some(key) = key {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: Some(key),
                    headers: None,
                    base_url: None,
                },
                env: None,
                source: Some(
                    if credential_key.is_some() {
                        "stored credential"
                    } else {
                        "GOOGLE_CLOUD_API_KEY"
                    }
                    .to_owned(),
                ),
            }));
        }

        let adc_path = match credential
            .and_then(|credential| credential.env.as_ref())
            .and_then(|env| env.get("GOOGLE_APPLICATION_CREDENTIALS"))
        {
            Some(path) => Some(path.clone()),
            None => ctx.env("GOOGLE_APPLICATION_CREDENTIALS").await,
        };
        let has_credentials = ctx
            .file_exists(adc_path.as_deref().unwrap_or(VERTEX_ADC_PATH))
            .await;
        let project = match credential
            .and_then(|credential| credential.env.as_ref())
            .and_then(|env| env.get("GOOGLE_CLOUD_PROJECT"))
        {
            Some(project) => Some(project.clone()),
            None => match ctx.env("GOOGLE_CLOUD_PROJECT").await {
                Some(project) => Some(project),
                None => ctx.env("GCLOUD_PROJECT").await,
            },
        };
        let location = match credential
            .and_then(|credential| credential.env.as_ref())
            .and_then(|env| env.get("GOOGLE_CLOUD_LOCATION"))
        {
            Some(location) => Some(location.clone()),
            None => ctx.env("GOOGLE_CLOUD_LOCATION").await,
        };
        // JS truthiness on all three conditions.
        if has_credentials
            && project
                .as_deref()
                .is_some_and(|project| !project.is_empty())
            && location
                .as_deref()
                .is_some_and(|location| !location.is_empty())
        {
            return Ok(Some(AuthResult {
                auth: ModelAuth {
                    api_key: None,
                    headers: None,
                    base_url: None,
                },
                env: credential.and_then(|credential| credential.env.clone()),
                source: Some(
                    if credential.is_some() {
                        "stored credential"
                    } else {
                        "gcloud application default credentials"
                    }
                    .to_owned(),
                ),
            }));
        }
        Ok(None)
    }
}

/// `googleVertexProvider()`.
pub fn google_vertex_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "google-vertex".to_owned(),
        name: Some("Google Vertex AI".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(Arc::new(VertexAuth)),
            oauth: None,
        },
        models: get_builtin_models("google-vertex").to_vec(),
        api: ProviderApi::Single(Arc::new(GoogleVertex)),
        ..Default::default()
    })
}
