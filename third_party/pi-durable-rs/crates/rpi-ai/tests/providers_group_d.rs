//! T13 W4 group D provider factories — port of the upstream test intent of
//! `packages/ai/test/providers.test.ts` (factory shape + env auth),
//! `xiaomi-models.test.ts`, `together-models.test.ts`,
//! `fireworks-models.test.ts` and `xai-responses.test.ts` (catalog-shape
//! parts) @ pi 0.82.1 (2efa728).
//!
//! The xiaomi-token-plan-ams anthropic empty-signature smoke test is a live
//! test of the anthropic-messages adapter against a hand-built model (not the
//! factory or the catalog); its adapter-side intent is covered by the
//! anthropic contract tests, so there is nothing factory-level to port.

use std::collections::HashMap;
use std::sync::Arc;

use rpi_ai::auth::{AuthContext, find_env_keys};
use rpi_ai::models::{CreateModelsOptions, Provider, create_models, get_supported_thinking_levels};
use rpi_ai::providers::cerebras::cerebras_provider;
use rpi_ai::providers::deepseek::deepseek_provider;
use rpi_ai::providers::fireworks::fireworks_provider;
use rpi_ai::providers::get_builtin_provider_spec;
use rpi_ai::providers::groq::groq_provider;
use rpi_ai::providers::huggingface::huggingface_provider;
use rpi_ai::providers::nvidia::nvidia_provider;
use rpi_ai::providers::together::together_provider;
use rpi_ai::providers::xai::xai_provider;
use rpi_ai::providers::xiaomi::xiaomi_provider;
use rpi_ai::providers::xiaomi_token_plan_ams::xiaomi_token_plan_ams_provider;
use rpi_ai::providers::xiaomi_token_plan_cn::xiaomi_token_plan_cn_provider;
use rpi_ai::providers::xiaomi_token_plan_sgp::xiaomi_token_plan_sgp_provider;
use rpi_ai::types::{
    ApiKind, InputModality, MaxTokensField, Model, ModelCompat, ModelThinkingLevel, ThinkingFormat,
    ThinkingLevelMap,
};

/// Factory shape expectations transcribed from the upstream
/// `providers/<id>.ts` files.
struct FactorySpec {
    factory: fn() -> Arc<dyn Provider>,
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    /// `envApiKeyAuth` display name.
    auth_name: &'static str,
    env_key: &'static str,
    /// Vendored catalog model count.
    model_count: usize,
}

#[allow(clippy::too_many_arguments)]
const fn spec(
    factory: fn() -> Arc<dyn Provider>,
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    auth_name: &'static str,
    env_key: &'static str,
    model_count: usize,
) -> FactorySpec {
    FactorySpec {
        factory,
        id,
        name,
        base_url,
        auth_name,
        env_key,
        model_count,
    }
}

const FACTORIES: [FactorySpec; 12] = [
    spec(
        cerebras_provider,
        "cerebras",
        "Cerebras",
        "https://api.cerebras.ai/v1",
        "Cerebras API key",
        "CEREBRAS_API_KEY",
        2,
    ),
    spec(
        deepseek_provider,
        "deepseek",
        "DeepSeek",
        "https://api.deepseek.com",
        "DeepSeek API key",
        "DEEPSEEK_API_KEY",
        2, // rc.13 catalog refresh: v4-flash/v4-flash-vision-exp consolidated to deepseek-flash
    ),
    spec(
        fireworks_provider,
        "fireworks",
        "Fireworks",
        "https://api.fireworks.ai/inference",
        "Fireworks API key",
        "FIREWORKS_API_KEY",
        24, // 2026-10-19 snapshot (+DeepSeek V4.1 Flash 1017, +Muse Spark Open)
    ),
    spec(
        groq_provider,
        "groq",
        "Groq",
        "https://api.groq.com/openai/v1",
        "Groq API key",
        "GROQ_API_KEY",
        7,
    ),
    spec(
        huggingface_provider,
        "huggingface",
        "Hugging Face",
        "https://router.huggingface.co/v1",
        "Hugging Face token",
        "HF_TOKEN",
        78, // 2026-10-12 rules regen @ abe508e1b (+MiniMax-M1-80k, +Qwen3-VL-30B-A3B-Instruct)
    ),
    spec(
        nvidia_provider,
        "nvidia",
        "NVIDIA",
        "https://integrate.api.nvidia.com/v1",
        "NVIDIA API key",
        "NVIDIA_API_KEY",
        21, // 2026-10-08 catalog refresh (deepseek-v4.1-flash + diffusiongemma-26b)
    ),
    spec(
        together_provider,
        "together",
        "Together",
        "https://api.together.ai/v1",
        "Together API key",
        "TOGETHER_API_KEY",
        20, // 2026-10-19 snapshot (+Muse Spark Open, +Nemotron 3.5 Super VL, +Beam 1.0)
    ),
    spec(
        xai_provider,
        "xai",
        "xAI",
        "https://api.x.ai/v1",
        "xAI API key",
        "XAI_API_KEY",
        4, // 19451accd rules regen: +grok-4.7 (models.dev snapshot 2026-09-23)
    ),
    spec(
        xiaomi_provider,
        "xiaomi",
        "Xiaomi",
        "https://api.xiaomimimo.com/v1",
        "Xiaomi API key",
        "XIAOMI_API_KEY",
        3, // 2026-10-19 snapshot: the mimo-v2.5 family retired (v2.6 only)
    ),
    spec(
        xiaomi_token_plan_ams_provider,
        "xiaomi-token-plan-ams",
        "Xiaomi Token Plan AMS",
        "https://token-plan-ams.xiaomimimo.com/v1",
        "Xiaomi Token Plan AMS API key",
        "XIAOMI_TOKEN_PLAN_AMS_API_KEY",
        2, // 2026-10-19 snapshot: the mimo-v2.5 family retired (v2.6 only)
    ),
    spec(
        xiaomi_token_plan_cn_provider,
        "xiaomi-token-plan-cn",
        "Xiaomi Token Plan CN",
        "https://token-plan-cn.xiaomimimo.com/v1",
        "Xiaomi Token Plan CN API key",
        "XIAOMI_TOKEN_PLAN_CN_API_KEY",
        2, // 2026-10-19 snapshot: the mimo-v2.5 family retired (v2.6 only)
    ),
    spec(
        xiaomi_token_plan_sgp_provider,
        "xiaomi-token-plan-sgp",
        "Xiaomi Token Plan SGP",
        "https://token-plan-sgp.xiaomimimo.com/v1",
        "Xiaomi Token Plan SGP API key",
        "XIAOMI_TOKEN_PLAN_SGP_API_KEY",
        2, // 2026-10-19 snapshot: the mimo-v2.5 family retired (v2.6 only)
    ),
];

fn get_model(provider: &Arc<dyn Provider>, id: &str) -> Model {
    provider
        .get_models()
        .into_iter()
        .find(|model| model.id == id)
        .unwrap_or_else(|| panic!("{} has model {id}", provider.id()))
}

/// Fixed-env `AuthContext` (upstream `fakeAuthContext` in providers.test.ts).
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

#[test]
fn factory_basics_match_upstream() {
    for expected in FACTORIES {
        let id = expected.id;
        let provider = (expected.factory)();
        assert_eq!(provider.id(), id);
        assert_eq!(provider.name(), expected.name);
        assert_eq!(
            provider.base_url(),
            Some(expected.base_url),
            "{id} base URL"
        );
        assert!(provider.headers().is_none(), "{id} default headers");
        let auth = provider
            .auth()
            .api_key
            .as_ref()
            .unwrap_or_else(|| panic!("{id} api-key auth"));
        assert_eq!(auth.name(), expected.auth_name, "{id} auth display name");

        let models = provider.get_models();
        assert_eq!(
            models.len(),
            expected.model_count,
            "{id} catalog model count"
        );
        assert!(
            models.iter().all(|model| model.provider == id),
            "{id} models carry the provider id"
        );

        // Registry entry wired to this factory.
        let registry = get_builtin_provider_spec(id).unwrap_or_else(|| panic!("{id} spec"));
        assert!(registry.factory.is_some(), "{id} factory registered");

        // env-api-keys discovery table agrees with the factory auth
        // (upstream together/fireworks-models.test.ts `findEnvKeys`).
        let env = HashMap::from([(expected.env_key.to_owned(), "test-key".to_owned())]);
        assert_eq!(
            find_env_keys(id, Some(&env)),
            Some(vec![expected.env_key.to_owned()]),
            "{id} env key discovery"
        );
    }
}

#[tokio::test]
async fn env_api_key_resolves_through_models_get_auth() {
    for expected in FACTORIES {
        let id = expected.id;
        let env_key = expected.env_key;
        let models = create_models(Some(CreateModelsOptions {
            credentials: None,
            auth_context: Some(Arc::new(MapAuthContext(HashMap::from([(
                env_key.to_owned(),
                "test-key".to_owned(),
            )])))),
            models_store: None,
        }));
        models.set_provider((expected.factory)());
        let model = models
            .get_models(Some(id))
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("{id} models"));
        let result = models
            .get_auth(&model, None)
            .await
            .expect("resolve")
            .unwrap_or_else(|| panic!("{id} configured"));
        assert_eq!(result.auth.api_key.as_deref(), Some("test-key"), "{id}");
        assert_eq!(result.source.as_deref(), Some(env_key), "{id} source");

        // Without the env var the provider is not configured.
        let unconfigured = create_models(Some(CreateModelsOptions {
            credentials: None,
            auth_context: Some(Arc::new(MapAuthContext(HashMap::new()))),
            models_store: None,
        }));
        unconfigured.set_provider((expected.factory)());
        let model = unconfigured
            .get_models(Some(id))
            .into_iter()
            .next()
            .unwrap();
        assert!(
            unconfigured
                .get_auth(&model, None)
                .await
                .expect("resolve")
                .is_none(),
            "{id} unconfigured without env key"
        );
    }
}

/// Upstream `xiaomi-models.test.ts` @ 9841914 (0e4d49541): deprecated
/// MiMo V2 ids (mimo-v2-flash/-omni/-v2-pro) are omitted from every Xiaomi
/// catalog; the mimo-v2.5 replacements stay.
#[test]
fn xiaomi_catalog_splits_api_billing_only_models() {
    let xiaomi = xiaomi_provider();
    let xiaomi_models = xiaomi.get_models();
    for id in ["mimo-v2-flash", "mimo-v2-omni", "mimo-v2-pro"] {
        let ids: Vec<&str> = xiaomi_models
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        assert!(!ids.contains(&id), "xiaomi deprecated {id} leaked");
    }
    let replacement = get_model(&xiaomi, "mimo-v2.6-flash");
    assert_eq!(replacement.api.as_str(), ApiKind::OPENAI_COMPLETIONS);
    assert_eq!(replacement.base_url, "https://api.xiaomimimo.com/v1");

    for factory in [
        xiaomi_token_plan_ams_provider,
        xiaomi_token_plan_cn_provider,
        xiaomi_token_plan_sgp_provider,
    ] {
        let provider = factory();
        let models = provider.get_models();
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        assert!(!ids.contains(&"mimo-v2-flash"), "{} flash", provider.id());
        assert!(!ids.contains(&"mimo-v2-omni"), "{} omni", provider.id());
        assert!(!ids.contains(&"mimo-v2-pro"), "{} v2-pro", provider.id());
        assert!(ids.contains(&"mimo-v2.6-pro"), "{} v2.6-pro", provider.id());
        assert!(
            models
                .iter()
                .all(|model| model.api.as_str() == ApiKind::OPENAI_COMPLETIONS),
            "{} single API",
            provider.id()
        );
    }
}

/// Upstream `together-models.test.ts`: the default Kimi K3 entry.
#[test]
fn together_kimi_k3_catalog_entry() {
    let provider = together_provider();
    let model = get_model(&provider, "moonshotai/Kimi-K3");

    assert_eq!(model.api.as_str(), ApiKind::OPENAI_COMPLETIONS);
    assert_eq!(model.provider, "together");
    assert_eq!(model.base_url, "https://api.together.ai/v1");
    assert!(model.reasoning);
    assert_eq!(
        model.thinking_level_map,
        Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Minimal, None),
            (ModelThinkingLevel::Low, None),
            (ModelThinkingLevel::Medium, None),
        ]))
    );
    assert_eq!(model.input, [InputModality::Text, InputModality::Image]);
    assert_eq!(model.context_window, 1048576);
    assert_eq!(model.max_tokens, 131072);
    assert_eq!(model.cost.rates.input, 3.0);
    assert_eq!(model.cost.rates.output, 15.0);
    assert_eq!(model.cost.rates.cache_read, 0.3);
    assert_eq!(model.cost.rates.cache_write, 0.0);
    assert_eq!(
        model.compat,
        Some(ModelCompat {
            supports_store: Some(false),
            supports_developer_role: Some(false),
            supports_reasoning_effort: Some(false),
            max_tokens_field: Some(MaxTokensField::MaxTokens),
            thinking_format: Some(ThinkingFormat::Together),
            supports_strict_mode: Some(false),
            supports_long_cache_retention: Some(false),
            ..ModelCompat::default()
        })
    );
}

/// Upstream `together-models.test.ts`: reasoning controls per model family.
#[test]
fn together_reasoning_controls_match_api_surface() {
    let provider = together_provider();

    let gpt_oss = get_model(&provider, "openai/gpt-oss-120b");
    assert_eq!(
        gpt_oss.thinking_level_map,
        Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Off, None),
            (ModelThinkingLevel::Minimal, None),
            (ModelThinkingLevel::Low, Some("low".to_owned())),
            (ModelThinkingLevel::Medium, Some("medium".to_owned())),
            (ModelThinkingLevel::High, Some("high".to_owned())),
            (ModelThinkingLevel::Max, None),
            (ModelThinkingLevel::Xhigh, None),
        ]))
    );
    let compat = gpt_oss.compat.as_ref().expect("gpt-oss compat");
    assert_eq!(compat.supports_reasoning_effort, Some(true));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Openai));

    let deepseek_v4 = get_model(&provider, "deepseek-ai/DeepSeek-V4-Pro-0813");
    // 2026-10-12 rules regen @ abe508e1b: v1.0.1 #10336 followed the
    // models.dev rename, so `TOGETHER_TOGGLE_REASONING_EFFORT_MODELS` now
    // names the -0813 sibling — the toggle map gains `high: "high"` while
    // the other levels stay off.
    assert_eq!(
        deepseek_v4.thinking_level_map,
        Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Minimal, None),
            (ModelThinkingLevel::Low, None),
            (ModelThinkingLevel::Medium, None),
            (ModelThinkingLevel::High, Some("high".to_owned())),
            (ModelThinkingLevel::Xhigh, None),
        ]))
    );
    let compat = deepseek_v4.compat.as_ref().expect("deepseek compat");
    assert_eq!(compat.supports_reasoning_effort, Some(true));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Together));

    let minimax = get_model(&provider, "MiniMaxAI/MiniMax-M2.7");
    assert_eq!(
        minimax.thinking_level_map,
        Some(ThinkingLevelMap::from([
            (ModelThinkingLevel::Off, None),
            (ModelThinkingLevel::Minimal, None),
            (ModelThinkingLevel::Low, None),
            (ModelThinkingLevel::Medium, None),
        ]))
    );
    let compat = minimax.compat.as_ref().expect("minimax compat");
    assert_eq!(compat.thinking_format, None);
    assert_eq!(compat.supports_reasoning_effort, Some(false));
}

/// Upstream `fireworks-models.test.ts`: non-GLM, non-Kimi-K3 models use the
/// Anthropic-compatible Messages API.
#[test]
fn fireworks_anthropic_catalog_entry() {
    let provider = fireworks_provider();
    let model = get_model(&provider, "accounts/fireworks/models/deepseek-v4p1-flash");

    assert_eq!(model.api.as_str(), ApiKind::ANTHROPIC_MESSAGES);
    assert_eq!(model.provider, "fireworks");
    assert_eq!(model.base_url, "https://api.fireworks.ai/inference");
    assert!(model.reasoning);
    assert_eq!(model.input, [InputModality::Text, InputModality::Image]);
}

/// Upstream `fireworks-models.test.ts` @ abe508e1b: the GLM 5.3 Fast router
/// is aligned with the base model's config (both on openai-completions).
#[test]
fn fireworks_router_models_align_with_base() {
    let provider = fireworks_provider();
    let base = get_model(&provider, "accounts/fireworks/models/glm-5p3");
    let fast = get_model(&provider, "accounts/fireworks/routers/glm-5p3-fast");
    assert_eq!(fast.api, base.api);
    assert_eq!(fast.base_url, base.base_url);
    assert_eq!(fast.compat, base.compat);
    assert_eq!(fast.thinking_level_map, base.thinking_level_map);
}

/// Mixed-API factory: the catalog carries both API kinds and the factory's
/// api map covers exactly those (dispatch itself is pinned in models.rs).
#[test]
fn fireworks_and_xai_catalogs_use_both_mapped_apis() {
    let fireworks = fireworks_provider();
    let models = fireworks.get_models();
    let apis: std::collections::BTreeSet<&str> =
        models.iter().map(|model| model.api.as_str()).collect();
    assert_eq!(
        apis,
        std::collections::BTreeSet::from([
            ApiKind::ANTHROPIC_MESSAGES,
            ApiKind::OPENAI_COMPLETIONS
        ])
    );

    let xai = xai_provider();
    let models = xai.get_models();
    let apis: std::collections::BTreeSet<&str> =
        models.iter().map(|model| model.api.as_str()).collect();
    // 70e878d4c: every built-in xAI model routes through Responses (the
    // openai-completions bucket is gone from the regenerated catalog).
    assert_eq!(
        apis,
        std::collections::BTreeSet::from([ApiKind::OPENAI_RESPONSES])
    );
}

/// Upstream `xai-responses.test.ts`: retired/redundant models stay out of the
/// built-in catalog.
#[test]
fn xai_catalog_excludes_retired_models() {
    let xai = xai_provider();
    let models = xai.get_models();
    let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
    for retired in [
        "grok-3",
        "grok-3-fast",
        "grok-4.20-0309-non-reasoning",
        "grok-4.20-0309-reasoning",
        "grok-code-fast-1",
    ] {
        assert!(!ids.contains(&retired), "retired {retired}");
    }
}

/// Upstream `xai-responses.test.ts` @ 9841914: every built-in xAI model
/// routes through Responses; thinking levels per model family.
#[test]
fn xai_grok_4_5_responses_thinking_levels() {
    let xai = xai_provider();

    let grok_4_5 = get_model(&xai, "grok-4.5");
    assert_eq!(grok_4_5.api.as_str(), ApiKind::OPENAI_RESPONSES);
    assert_eq!(grok_4_5.base_url, "https://api.x.ai/v1");
    assert_eq!(
        get_supported_thinking_levels(&grok_4_5),
        [
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );

    let grok_4_6 = get_model(&xai, "grok-4.6");
    assert_eq!(
        get_supported_thinking_levels(&grok_4_6),
        [
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
            ModelThinkingLevel::Xhigh,
        ]
    );

    let grok_4_3 = get_model(&xai, "grok-4.3");
    assert_eq!(grok_4_3.api.as_str(), ApiKind::OPENAI_RESPONSES);
    assert_eq!(
        get_supported_thinking_levels(&grok_4_3),
        [
            ModelThinkingLevel::Off,
            ModelThinkingLevel::Low,
            ModelThinkingLevel::Medium,
            ModelThinkingLevel::High,
        ]
    );
}

/// Spot-check the single-API factories route every catalog model through
/// openai-completions.
#[test]
fn single_api_factories_are_openai_completions_only() {
    for factory in [
        cerebras_provider,
        deepseek_provider,
        groq_provider,
        huggingface_provider,
        nvidia_provider,
        together_provider,
        xiaomi_provider,
        xiaomi_token_plan_ams_provider,
        xiaomi_token_plan_cn_provider,
        xiaomi_token_plan_sgp_provider,
    ] {
        let provider = factory();
        assert!(
            provider
                .get_models()
                .iter()
                .all(|model| model.api.as_str() == ApiKind::OPENAI_COMPLETIONS),
            "{} models all openai-completions",
            provider.id()
        );
    }
}

/// The xAI OAuth login (SuperGrok / X Premium) is wired into the factory
/// slot (T13 W5, deviation D-031 closed; `providers/xai.rs`).
#[test]
fn xai_oauth_slot_wired() {
    let xai = xai_provider();
    let oauth = xai.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "xAI (Grok/X subscription)");
}
