//! Port of `packages/ai/src/providers/cloudflare-workers-ai.ts` @ pi 0.82.1
//! (2efa728) — Cloudflare Workers AI: OpenAI Completions transport, wrapped
//! so the `{CLOUDFLARE_ACCOUNT_ID}` base-URL placeholder materializes from
//! the resolved provider env before dispatch (see `cloudflare_stream`).
//!
//! Auth (`cloudflare-auth.ts`) lands in `crate::auth::cloudflare_auth`:
//! `CLOUDFLARE_API_KEY` + `CLOUDFLARE_ACCOUNT_ID`, request auth via api key.

use std::sync::Arc;

use crate::api::cloudflare_workers_ai_system_one::{
    CLOUDFLARE_WORKERS_AI_SYSTEM_ONE_API, cloudflare_workers_ai_system_one_api,
};
use crate::api::openai_completions::OpenAiCompletions;
use crate::auth::ProviderAuth;
use crate::auth::cloudflare_auth::cloudflare_workers_ai_auth;
use crate::generated::{get_builtin_all_models, get_builtin_models};
use crate::models::{CreateProviderOptions, Provider, ProviderApi, create_provider};
use crate::types::{
    ClassifierContext, ClassifierModel, ClassifierOptions, ClassifierResult, ProviderClassifier,
    ProviderEnv,
};

use super::cloudflare_stream::cloudflare_streams;

/// `cloudflareClassifier` (cloudflare-stream.ts:31-35): materialize the
/// `{CLOUDFLARE_ACCOUNT_ID}` placeholder in the classifier model base URL
/// from the resolved provider env before delegating.
struct CloudflareClassifier {
    inner: std::sync::Arc<dyn ProviderClassifier>,
}

fn resolve_cloudflare_classifier(
    model: &ClassifierModel,
    env: Option<&ProviderEnv>,
) -> ClassifierModel {
    let Some(env) = env else {
        return model.clone();
    };
    let account_id = env
        .get("CLOUDFLARE_ACCOUNT_ID")
        .map(String::as_str)
        .unwrap_or("{CLOUDFLARE_ACCOUNT_ID}");
    let base_url = model
        .base_url
        .replace("{CLOUDFLARE_ACCOUNT_ID}", account_id);
    if base_url == model.base_url {
        model.clone()
    } else {
        ClassifierModel {
            base_url,
            ..model.clone()
        }
    }
}

impl ProviderClassifier for CloudflareClassifier {
    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ClassifierResult> + Send + 'static>>
    {
        let model = resolve_cloudflare_classifier(model, options.and_then(|o| o.env.as_ref()));
        let context = context.clone();
        let options = options.cloned();
        let inner = self.inner.clone();
        Box::pin(async move { inner.classify(&model, &context, options.as_ref()).await })
    }
}

/// `cloudflareWorkersAIProvider()`.
pub fn cloudflare_workers_ai_provider() -> Arc<dyn Provider> {
    create_provider(CreateProviderOptions {
        id: "cloudflare-workers-ai".to_owned(),
        name: Some("Cloudflare Workers AI".to_owned()),
        base_url: None,
        headers: None,
        auth: ProviderAuth {
            api_key: Some(cloudflare_workers_ai_auth()),
            oauth: None,
        },
        models: get_builtin_models("cloudflare-workers-ai").to_vec(),
        all_models: get_builtin_all_models("cloudflare-workers-ai").to_vec(),
        classifiers: std::collections::HashMap::from([(
            CLOUDFLARE_WORKERS_AI_SYSTEM_ONE_API.to_owned(),
            std::sync::Arc::new(CloudflareClassifier {
                inner: cloudflare_workers_ai_system_one_api(),
            }) as std::sync::Arc<dyn ProviderClassifier>,
        )]),
        api: ProviderApi::Single(cloudflare_streams(Arc::new(OpenAiCompletions))),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ApiKind;

    #[test]
    fn catalog_base_urls_carry_the_account_placeholder() {
        let models = get_builtin_models("cloudflare-workers-ai");
        assert!(!models.is_empty());
        for model in models {
            assert_eq!(model.api.as_str(), ApiKind::OPENAI_COMPLETIONS);
            assert!(
                model.base_url.contains("{CLOUDFLARE_ACCOUNT_ID}"),
                "{}: {}",
                model.id,
                model.base_url
            );
        }
    }

    #[test]
    fn classifier_materializes_the_account_placeholder() {
        let models = get_builtin_all_models("cloudflare-workers-ai");
        let classifier = models
            .iter()
            .find_map(|model| match model {
                crate::types::AnyModel::Classifier(classifier) => Some(classifier),
                _ => None,
            })
            .expect("classifier catalog entry");
        let env = ProviderEnv::from([("CLOUDFLARE_ACCOUNT_ID".to_owned(), "account".to_owned())]);
        let resolved = resolve_cloudflare_classifier(classifier, Some(&env));
        assert!(
            resolved.base_url.contains("/accounts/account/ai"),
            "{}",
            resolved.base_url
        );
        assert!(!resolved.base_url.contains("{CLOUDFLARE_ACCOUNT_ID}"));
    }
}
