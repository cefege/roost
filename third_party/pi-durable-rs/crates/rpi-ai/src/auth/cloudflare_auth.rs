//! Port of `packages/ai/src/providers/cloudflare-auth.ts` @ pi 0.82.1
//! (2efa728) — Cloudflare API-key auth shared by the `cloudflare-workers-ai`
//! and `cloudflare-ai-gateway` providers.
//!
//! Both kinds resolve `CLOUDFLARE_API_KEY` + `CLOUDFLARE_ACCOUNT_ID` (the AI
//! Gateway kind also `CLOUDFLARE_GATEWAY_ID`) with a per-field merge — a
//! credential carrying only the API key still picks up the account/gateway id
//! from the ambient env. The AI Gateway kind authenticates via the
//! `cf-aig-authorization` header and suppresses the default `Authorization` /
//! `x-api-key` headers (`None` values, headers.ts null semantics).

use std::sync::Arc;

use async_trait::async_trait;

use super::interaction::{AuthInteraction, AuthPrompt};
use super::resolve::ModelsError;
use super::types::{ApiKeyAuth, ApiKeyCredential, AuthContext, AuthResult, ModelAuth};
use crate::types::ProviderEnv;

pub const CLOUDFLARE_API_KEY: &str = "CLOUDFLARE_API_KEY";
pub const CLOUDFLARE_ACCOUNT_ID: &str = "CLOUDFLARE_ACCOUNT_ID";
pub const CLOUDFLARE_GATEWAY_ID: &str = "CLOUDFLARE_GATEWAY_ID";

/// `CloudflareAuthKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloudflareAuthKind {
    WorkersAi,
    AiGateway,
}

/// `resolveValue` — per-field merge: prefer the credential value, fall back
/// to ambient env.
async fn resolve_value(
    name: &str,
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
) -> Option<String> {
    let from_credential = credential.and_then(|credential| {
        if name == CLOUDFLARE_API_KEY {
            credential.key.clone()
        } else {
            credential.env.as_ref()?.get(name).cloned()
        }
    });
    match from_credential {
        Some(value) => Some(value),
        None => ctx.env(name).await,
    }
}

struct ResolvedCloudflareEnv {
    api_key: String,
    env: ProviderEnv,
    source: String,
}

/// `resolveCloudflareEnv` — `None` unless every required value resolved
/// (JS falsy check: empty strings count as missing).
async fn resolve_cloudflare_env(
    kind: CloudflareAuthKind,
    ctx: &dyn AuthContext,
    credential: Option<&ApiKeyCredential>,
) -> Option<ResolvedCloudflareEnv> {
    let api_key = resolve_value(CLOUDFLARE_API_KEY, ctx, credential)
        .await
        .filter(|value| !value.is_empty())?;
    let account_id = resolve_value(CLOUDFLARE_ACCOUNT_ID, ctx, credential)
        .await
        .filter(|value| !value.is_empty())?;
    let gateway_id = match kind {
        CloudflareAuthKind::AiGateway => Some(
            resolve_value(CLOUDFLARE_GATEWAY_ID, ctx, credential)
                .await
                .filter(|value| !value.is_empty())?,
        ),
        CloudflareAuthKind::WorkersAi => None,
    };

    let mut env = ProviderEnv::from([(CLOUDFLARE_ACCOUNT_ID.to_owned(), account_id)]);
    if let Some(gateway_id) = gateway_id {
        env.insert(CLOUDFLARE_GATEWAY_ID.to_owned(), gateway_id);
    }
    Some(ResolvedCloudflareEnv {
        api_key,
        env,
        source: if credential.is_some() {
            "stored credential".to_owned()
        } else {
            CLOUDFLARE_API_KEY.to_owned()
        },
    })
}

/// `cloudflareWorkersAIAuth` / `cloudflareAIGatewayAuth` — one type, two
/// kinds (the upstream factories share every code path but the gateway id).
pub struct CloudflareAuth {
    kind: CloudflareAuthKind,
}

/// `cloudflareWorkersAIAuth()`.
pub fn cloudflare_workers_ai_auth() -> Arc<CloudflareAuth> {
    Arc::new(CloudflareAuth {
        kind: CloudflareAuthKind::WorkersAi,
    })
}

/// `cloudflareAIGatewayAuth()`.
pub fn cloudflare_ai_gateway_auth() -> Arc<CloudflareAuth> {
    Arc::new(CloudflareAuth {
        kind: CloudflareAuthKind::AiGateway,
    })
}

#[async_trait]
impl ApiKeyAuth for CloudflareAuth {
    fn name(&self) -> &str {
        "Cloudflare API key"
    }

    fn supports_login(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<ApiKeyCredential, ModelsError> {
        let key = interaction
            .prompt(AuthPrompt::secret("Enter Cloudflare API key"))
            .await?;
        let account_id = interaction
            .prompt(AuthPrompt::Text {
                message: "Enter Cloudflare account ID".to_owned(),
                placeholder: None,
                signal: None,
            })
            .await?;
        let mut env = ProviderEnv::from([(CLOUDFLARE_ACCOUNT_ID.to_owned(), account_id)]);
        if self.kind == CloudflareAuthKind::AiGateway {
            let gateway_id = interaction
                .prompt(AuthPrompt::Text {
                    message: "Enter Cloudflare AI Gateway ID".to_owned(),
                    placeholder: None,
                    signal: None,
                })
                .await?;
            env.insert(CLOUDFLARE_GATEWAY_ID.to_owned(), gateway_id);
        }
        Ok(ApiKeyCredential {
            key: Some(key),
            env: Some(env),
        })
    }

    async fn resolve(
        &self,
        ctx: &dyn AuthContext,
        credential: Option<&ApiKeyCredential>,
    ) -> Result<Option<AuthResult>, ModelsError> {
        let Some(resolved) = resolve_cloudflare_env(self.kind, ctx, credential).await else {
            return Ok(None);
        };
        let auth = match self.kind {
            CloudflareAuthKind::WorkersAi => ModelAuth {
                api_key: Some(resolved.api_key),
                headers: None,
                base_url: None,
            },
            CloudflareAuthKind::AiGateway => ModelAuth {
                api_key: None,
                headers: Some(crate::types::ProviderHeaders::from([
                    (
                        "cf-aig-authorization".to_owned(),
                        Some(format!("Bearer {}", resolved.api_key)),
                    ),
                    ("Authorization".to_owned(), None),
                    ("x-api-key".to_owned(), None),
                ])),
                base_url: None,
            },
        };
        Ok(Some(AuthResult {
            auth,
            env: Some(resolved.env),
            source: Some(resolved.source),
        }))
    }
}

#[cfg(test)]
mod tests {
    //! Port intent of the Cloudflare sections of `test/providers.test.ts`
    //! @ 2efa728: Workers AI requires key+account and returns scoped env; the
    //! AI Gateway kind additionally requires the gateway id and authenticates
    //! via `cf-aig-authorization`, suppressing the default auth headers.

    use std::collections::HashMap;

    use super::*;

    struct MapAuthContext(HashMap<String, String>);

    #[async_trait]
    impl AuthContext for MapAuthContext {
        async fn env(&self, name: &str) -> Option<String> {
            self.0.get(name).cloned()
        }

        async fn file_exists(&self, _path: &str) -> bool {
            false
        }
    }

    fn ctx(entries: &[(&str, &str)]) -> MapAuthContext {
        MapAuthContext(
            entries
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
        )
    }

    #[tokio::test]
    async fn workers_ai_requires_account_and_returns_scoped_env() {
        let auth = cloudflare_workers_ai_auth();
        let missing = ctx(&[(CLOUDFLARE_API_KEY, "cf-key")]);
        assert!(
            auth.resolve(&missing, None)
                .await
                .expect("resolve")
                .is_none()
        );

        let configured = ctx(&[
            (CLOUDFLARE_API_KEY, "cf-key"),
            (CLOUDFLARE_ACCOUNT_ID, "acct"),
        ]);
        let result = auth
            .resolve(&configured, None)
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("cf-key"));
        assert_eq!(result.auth.headers, None);
        assert_eq!(
            result.env,
            Some(ProviderEnv::from([(
                CLOUDFLARE_ACCOUNT_ID.to_owned(),
                "acct".to_owned()
            )]))
        );
        assert_eq!(result.source.as_deref(), Some(CLOUDFLARE_API_KEY));
    }

    #[tokio::test]
    async fn ai_gateway_requires_gateway_and_returns_scoped_env_headers() {
        let auth = cloudflare_ai_gateway_auth();
        let missing_gateway = ctx(&[
            (CLOUDFLARE_API_KEY, "cf-key"),
            (CLOUDFLARE_ACCOUNT_ID, "acct"),
        ]);
        assert!(
            auth.resolve(&missing_gateway, None)
                .await
                .expect("resolve")
                .is_none()
        );

        let configured = ctx(&[
            (CLOUDFLARE_API_KEY, "cf-key"),
            (CLOUDFLARE_ACCOUNT_ID, "acct"),
            (CLOUDFLARE_GATEWAY_ID, "gw"),
        ]);
        let result = auth
            .resolve(&configured, None)
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key, None);
        let headers = result.auth.headers.expect("headers");
        assert_eq!(
            headers.get("cf-aig-authorization"),
            Some(&Some("Bearer cf-key".to_owned()))
        );
        assert_eq!(headers.get("Authorization"), Some(&None));
        assert_eq!(headers.get("x-api-key"), Some(&None));
        assert_eq!(
            result.env,
            Some(ProviderEnv::from([
                (CLOUDFLARE_ACCOUNT_ID.to_owned(), "acct".to_owned()),
                (CLOUDFLARE_GATEWAY_ID.to_owned(), "gw".to_owned()),
            ]))
        );
    }

    #[tokio::test]
    async fn credential_key_only_still_picks_up_account_id_from_env() {
        // Per-field merge (cloudflare-auth.ts:14-24): a credential carrying
        // only the API key resolves the account id from ambient env.
        let auth = cloudflare_workers_ai_auth();
        let credential = ApiKeyCredential {
            key: Some("stored-key".to_owned()),
            env: None,
        };
        let ambient = ctx(&[(CLOUDFLARE_ACCOUNT_ID, "acct")]);
        let result = auth
            .resolve(&ambient, Some(&credential))
            .await
            .expect("resolve")
            .expect("configured");
        assert_eq!(result.auth.api_key.as_deref(), Some("stored-key"));
        assert_eq!(result.source.as_deref(), Some("stored credential"));
        assert_eq!(
            result.env,
            Some(ProviderEnv::from([(
                CLOUDFLARE_ACCOUNT_ID.to_owned(),
                "acct".to_owned()
            )]))
        );
    }
}
