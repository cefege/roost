//! Port of `packages/ai/src/providers/all.ts` @ pi 0.84.1 (`4181f66`) — T13 W4
//! phase 1: the built-in provider registry (41 factory ids, upstream
//! registration order) and the catalog data path.
//!
//! Factory implementations (`xxxProvider()` per upstream `providers/xxx.ts`)
//! landed in the follow-up W4 waves as `src/providers/<id>.rs` files.
//!
//! Intentional differences:
//! - Upstream `builtinProviders()` constructs all 41 providers; the Rust
//!   `builtin_providers()` does the same now that every W4 wave has landed
//!   (while waves were in flight it yielded only the ported subset).
//! - The `openrouter-images` image-generation provider is part of the images
//!   subsystem (W6), not this registry.

use std::sync::Arc;

use crate::models::{CreateModelsOptions, Models, Provider, create_models};

pub mod amazon_bedrock;
pub mod ant_ling;
pub mod anthropic;
pub mod azure;
pub mod baseten;
pub mod cerebras;
pub mod cloudflare_ai_gateway;
pub mod cloudflare_stream;
pub mod cloudflare_workers_ai;
pub mod deepseek;
pub mod fireworks;
pub mod github_copilot;
pub mod google;
pub mod google_vertex;
pub mod groq;
pub mod huggingface;
pub mod kimi_coding;
pub mod meta;
pub mod minimax;
pub mod minimax_cn;
pub mod mistral;
pub mod moonshotai;
pub mod moonshotai_cn;
pub mod nvidia;
pub mod openai;
pub mod openai_codex;
pub mod opencode;
pub mod opencode_go;
pub mod openrouter;
pub mod qwen_token_plan;
pub mod qwen_token_plan_cn;
pub mod qwen_token_plan_individual;
pub mod radius;
pub mod radius_config;
pub mod together;
pub mod typesafe;
pub mod vercel_ai_gateway;
pub mod xai;
pub mod xiaomi;
pub mod xiaomi_token_plan_ams;
pub mod xiaomi_token_plan_cn;
pub mod xiaomi_token_plan_sgp;
pub mod zai;
pub mod zai_coding_cn;

/// Upstream `xxxProvider()` factory: builds a fresh provider instance.
pub type ProviderFactory = fn() -> Arc<dyn Provider>;

/// Registry entry for one built-in provider.
pub struct BuiltinProviderSpec {
    /// Provider id (upstream `createProvider({ id })`).
    pub id: &'static str,
    /// Whether the provider has a static entry in the vendored catalog
    /// (`generated.rs`). Every built-in provider qualifies since the
    /// Radius public catalog shipped (4d38031fb); `meta` registered with
    /// its catalog in V15-15 (b73412a37).
    pub in_catalog: bool,
    /// Factory, once the provider is ported in a W4 follow-up wave.
    pub factory: Option<ProviderFactory>,
}

/// All 41 built-in providers in upstream `builtinProviders()` registration
/// order. Registration order is observable (insertion-ordered `Models`:
/// initial-model fallback, available-model listings) — do not reorder.
pub static BUILTIN_PROVIDERS: &[BuiltinProviderSpec] = &[
    BuiltinProviderSpec {
        id: "amazon-bedrock",
        in_catalog: true,
        factory: Some(amazon_bedrock::amazon_bedrock_provider),
    },
    BuiltinProviderSpec {
        id: "ant-ling",
        in_catalog: true,
        factory: Some(ant_ling::ant_ling_provider),
    },
    BuiltinProviderSpec {
        id: "anthropic",
        in_catalog: true,
        factory: Some(anthropic::anthropic_provider),
    },
    BuiltinProviderSpec {
        id: "azure",
        in_catalog: true,
        factory: Some(azure::azure_provider),
    },
    BuiltinProviderSpec {
        id: "baseten",
        in_catalog: true,
        factory: Some(baseten::baseten_provider),
    },
    BuiltinProviderSpec {
        id: "cerebras",
        in_catalog: true,
        factory: Some(cerebras::cerebras_provider),
    },
    BuiltinProviderSpec {
        id: "cloudflare-ai-gateway",
        in_catalog: true,
        factory: Some(cloudflare_ai_gateway::cloudflare_ai_gateway_provider),
    },
    BuiltinProviderSpec {
        id: "cloudflare-workers-ai",
        in_catalog: true,
        factory: Some(cloudflare_workers_ai::cloudflare_workers_ai_provider),
    },
    BuiltinProviderSpec {
        id: "deepseek",
        in_catalog: true,
        factory: Some(deepseek::deepseek_provider),
    },
    BuiltinProviderSpec {
        id: "fireworks",
        in_catalog: true,
        factory: Some(fireworks::fireworks_provider),
    },
    BuiltinProviderSpec {
        id: "github-copilot",
        in_catalog: true,
        factory: Some(github_copilot::github_copilot_provider),
    },
    BuiltinProviderSpec {
        id: "google",
        in_catalog: true,
        factory: Some(google::google_provider),
    },
    BuiltinProviderSpec {
        id: "google-vertex",
        in_catalog: true,
        factory: Some(google_vertex::google_vertex_provider),
    },
    BuiltinProviderSpec {
        id: "groq",
        in_catalog: true,
        factory: Some(groq::groq_provider),
    },
    BuiltinProviderSpec {
        id: "huggingface",
        in_catalog: true,
        factory: Some(huggingface::huggingface_provider),
    },
    BuiltinProviderSpec {
        id: "kimi-coding",
        in_catalog: true,
        factory: Some(kimi_coding::kimi_coding_provider),
    },
    BuiltinProviderSpec {
        id: "meta",
        in_catalog: true,
        factory: Some(meta::meta_provider),
    },
    BuiltinProviderSpec {
        id: "minimax",
        in_catalog: true,
        factory: Some(minimax::minimax_provider),
    },
    BuiltinProviderSpec {
        id: "minimax-cn",
        in_catalog: true,
        factory: Some(minimax_cn::minimax_cn_provider),
    },
    BuiltinProviderSpec {
        id: "mistral",
        in_catalog: true,
        factory: Some(mistral::mistral_provider),
    },
    BuiltinProviderSpec {
        id: "moonshotai",
        in_catalog: true,
        factory: Some(moonshotai::moonshotai_provider),
    },
    BuiltinProviderSpec {
        id: "moonshotai-cn",
        in_catalog: true,
        factory: Some(moonshotai_cn::moonshotai_cn_provider),
    },
    BuiltinProviderSpec {
        id: "nvidia",
        in_catalog: true,
        factory: Some(nvidia::nvidia_provider),
    },
    BuiltinProviderSpec {
        id: "openai",
        in_catalog: true,
        factory: Some(openai::openai_provider),
    },
    BuiltinProviderSpec {
        id: "openai-codex",
        in_catalog: true,
        factory: Some(openai_codex::openai_codex_provider),
    },
    BuiltinProviderSpec {
        id: "opencode",
        in_catalog: true,
        factory: Some(opencode::opencode_provider),
    },
    BuiltinProviderSpec {
        id: "opencode-go",
        in_catalog: true,
        factory: Some(opencode_go::opencode_go_provider),
    },
    BuiltinProviderSpec {
        id: "openrouter",
        in_catalog: true,
        factory: Some(openrouter::openrouter_provider),
    },
    BuiltinProviderSpec {
        id: "qwen-token-plan",
        in_catalog: true,
        factory: Some(qwen_token_plan::qwen_token_plan_provider),
    },
    BuiltinProviderSpec {
        id: "qwen-token-plan-cn",
        in_catalog: true,
        factory: Some(qwen_token_plan_cn::qwen_token_plan_cn_provider),
    },
    BuiltinProviderSpec {
        id: "qwen-token-plan-individual",
        in_catalog: true,
        factory: Some(qwen_token_plan_individual::qwen_token_plan_individual_provider),
    },
    BuiltinProviderSpec {
        id: "radius",
        // Static public catalog since 4d38031fb (27 models @ 19451accd);
        // the gateway overlay lives inside the provider itself.
        in_catalog: true,
        factory: Some(radius::radius_provider),
    },
    BuiltinProviderSpec {
        id: "together",
        in_catalog: true,
        factory: Some(together::together_provider),
    },
    BuiltinProviderSpec {
        // Classifier-only provider (schema v6): `typesafeProvider()` @
        // abe508e1b; upstream order = between together and vercel-ai-gateway.
        id: "typesafe",
        in_catalog: true,
        factory: Some(typesafe::typesafe_provider),
    },
    BuiltinProviderSpec {
        id: "vercel-ai-gateway",
        in_catalog: true,
        factory: Some(vercel_ai_gateway::vercel_ai_gateway_provider),
    },
    BuiltinProviderSpec {
        id: "xai",
        in_catalog: true,
        factory: Some(xai::xai_provider),
    },
    BuiltinProviderSpec {
        id: "xiaomi",
        in_catalog: true,
        factory: Some(xiaomi::xiaomi_provider),
    },
    BuiltinProviderSpec {
        id: "xiaomi-token-plan-ams",
        in_catalog: true,
        factory: Some(xiaomi_token_plan_ams::xiaomi_token_plan_ams_provider),
    },
    BuiltinProviderSpec {
        id: "xiaomi-token-plan-cn",
        in_catalog: true,
        factory: Some(xiaomi_token_plan_cn::xiaomi_token_plan_cn_provider),
    },
    BuiltinProviderSpec {
        id: "xiaomi-token-plan-sgp",
        in_catalog: true,
        factory: Some(xiaomi_token_plan_sgp::xiaomi_token_plan_sgp_provider),
    },
    BuiltinProviderSpec {
        id: "zai",
        in_catalog: true,
        factory: Some(zai::zai_provider),
    },
    BuiltinProviderSpec {
        id: "zai-coding-cn",
        in_catalog: true,
        factory: Some(zai_coding_cn::zai_coding_cn_provider),
    },
];

/// Ids of all built-in providers, upstream registration order.
pub fn builtin_provider_ids() -> impl Iterator<Item = &'static str> {
    BUILTIN_PROVIDERS.iter().map(|spec| spec.id)
}

/// Registry lookup by provider id.
pub fn get_builtin_provider_spec(id: &str) -> Option<&'static BuiltinProviderSpec> {
    BUILTIN_PROVIDERS.iter().find(|spec| spec.id == id)
}

/// `builtinProviders()` — freshly constructed providers for every built-in
/// factory, in registration order.
pub fn builtin_providers() -> Vec<Arc<dyn Provider>> {
    BUILTIN_PROVIDERS
        .iter()
        .filter_map(|spec| spec.factory.map(|factory| factory()))
        .collect()
}

/// `builtinModels(options)` — a `Models` collection with every built-in
/// provider registered.
pub fn builtin_models(options: Option<CreateModelsOptions>) -> Models {
    let models = create_models(options);
    for provider in builtin_providers() {
        models.set_provider(provider);
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generated::{builtin_catalog, get_builtin_models};

    /// Upstream `builtinProviders()` order, transcribed from `all.ts` @ 4181f66.
    const UPSTREAM_ORDER: [&str; 42] = [
        "amazon-bedrock",
        "ant-ling",
        "anthropic",
        "azure",
        "baseten",
        "cerebras",
        "cloudflare-ai-gateway",
        "cloudflare-workers-ai",
        "deepseek",
        "fireworks",
        "github-copilot",
        "google",
        "google-vertex",
        "groq",
        "huggingface",
        "kimi-coding",
        "meta",
        "minimax",
        "minimax-cn",
        "mistral",
        "moonshotai",
        "moonshotai-cn",
        "nvidia",
        "openai",
        "openai-codex",
        "opencode",
        "opencode-go",
        "openrouter",
        "qwen-token-plan",
        "qwen-token-plan-cn",
        "qwen-token-plan-individual",
        "radius",
        "together",
        "typesafe",
        "vercel-ai-gateway",
        "xai",
        "xiaomi",
        "xiaomi-token-plan-ams",
        "xiaomi-token-plan-cn",
        "xiaomi-token-plan-sgp",
        "zai",
        "zai-coding-cn",
    ];

    #[test]
    fn test_registry_matches_upstream_all_ts() {
        let ids: Vec<&str> = builtin_provider_ids().collect();
        assert_eq!(ids, UPSTREAM_ORDER);
    }

    #[test]
    fn test_catalog_membership() {
        let catalog = builtin_catalog().expect("catalog");
        for spec in BUILTIN_PROVIDERS {
            assert!(
                catalog.providers().contains(&spec.id),
                "catalog entry missing for {}",
                spec.id
            );
            assert!(
                !get_builtin_models(spec.id).is_empty() || spec.id == "typesafe",
                "no catalog models for {}",
                spec.id
            );
        }
        // Since V15-15 every registered provider has a catalog entry; the v6
        // catalog additionally carries `typesafe` (classifier-only), whose
        // provider spec + system-one adapter landed with V16-07 (02 §1.2) —
        // the two sides now match one-to-one.
        assert_eq!(catalog.providers().len(), BUILTIN_PROVIDERS.len());
        assert!(catalog.providers().contains(&"meta"));
        assert!(catalog.providers().contains(&"typesafe"));
    }

    #[test]
    fn test_spec_lookup() {
        assert!(get_builtin_provider_spec("zai").is_some());
        assert!(get_builtin_provider_spec("radius").is_some());
        assert!(get_builtin_provider_spec("openrouter-images").is_none());
        assert!(get_builtin_provider_spec("nope").is_none());
    }

    #[test]
    fn test_builtin_providers_yields_ported_subset() {
        // W4 in flight: `builtin_providers()` yields only the ported subset.
        // Group C wave: 10 factories; group D wave: 12 more; group A wave:
        // 8 more; group B wave (this): 8 more. Other waves add their own ids
        // here as they land.
        const PORTED: [&str; 42] = [
            "amazon-bedrock",
            "ant-ling",
            "anthropic",
            "azure",
            "baseten",
            "cerebras",
            "cloudflare-ai-gateway",
            "cloudflare-workers-ai",
            "deepseek",
            "fireworks",
            "github-copilot",
            "google",
            "google-vertex",
            "groq",
            "huggingface",
            "kimi-coding",
            "meta",
            "minimax",
            "minimax-cn",
            "mistral",
            "moonshotai",
            "moonshotai-cn",
            "nvidia",
            "openai",
            "openai-codex",
            "opencode",
            "opencode-go",
            "openrouter",
            "qwen-token-plan",
            "qwen-token-plan-cn",
            "qwen-token-plan-individual",
            "radius",
            "together",
            "typesafe",
            "vercel-ai-gateway",
            "xai",
            "xiaomi",
            "xiaomi-token-plan-ams",
            "xiaomi-token-plan-cn",
            "xiaomi-token-plan-sgp",
            "zai",
            "zai-coding-cn",
        ];
        let providers = builtin_providers();
        let ids: Vec<&str> = providers.iter().map(|provider| provider.id()).collect();
        for id in &ids {
            assert!(PORTED.contains(id), "unexpected ported provider {id}");
        }
        for id in PORTED {
            assert!(ids.contains(&id), "ported provider {id} not yielded");
        }
        let models = builtin_models(None);
        assert_eq!(models.get_providers().len(), providers.len());
    }
}
