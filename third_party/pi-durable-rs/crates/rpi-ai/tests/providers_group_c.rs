//! T13 W4 phase 2 (group C) provider factory tests.
//!
//! Ports the group-C-relevant intent of upstream
//! `packages/ai/test/providers.test.ts` (factory shape + Moonshot/Kimi
//! pricing), `qwen-token-plan-models.test.ts` (text-only model list) and the
//! `zai` compat assertions of `openai-completions-tool-choice.test.ts`
//! (`zaiToolStream`) against the Rust factories in
//! `rpi_ai::providers::{zai, zai_coding_cn, moonshotai, moonshotai_cn,
//! kimi_coding, meta, minimax, minimax_cn, ant_ling, qwen_token_plan,
//! qwen_token_plan_cn}` @ pi 0.82.1 (2efa728) + meta @ 0.86.1
//! (b73412a37, V15-15).
//!
//! The OAuth part of `kimi-coding-oauth.test.ts` is ported in T13 W5 (see
//! `auth/oauth/kimi_coding.rs` and `tests/oauth_kimi_xai.rs`); the factory
//! slot is wired (deviation D-029 closed).

use std::collections::HashMap;
use std::sync::Arc;

use rpi_ai::auth::AuthContext;
use rpi_ai::models::Provider;
use rpi_ai::providers::{
    ant_ling, kimi_coding, meta, minimax, minimax_cn, moonshotai, moonshotai_cn, qwen_token_plan,
    qwen_token_plan_cn, zai, zai_coding_cn,
};
use rpi_ai::types::ApiKind;

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

/// Expected factory parameters, transcribed from the upstream
/// `providers/<id>.ts` files @ 2efa728.
struct FactorySpec {
    id: &'static str,
    name: &'static str,
    base_url: &'static str,
    auth_name: &'static str,
    env_key: &'static str,
    api: &'static str,
    factory: fn() -> Arc<dyn Provider>,
}

const SPECS: &[FactorySpec] = &[
    FactorySpec {
        id: "zai",
        name: "Z.AI",
        base_url: "https://api.z.ai/api/coding/paas/v4",
        auth_name: "Z.AI API key",
        env_key: "ZAI_API_KEY",
        api: ApiKind::OPENAI_COMPLETIONS,
        factory: zai::zai_provider,
    },
    FactorySpec {
        id: "zai-coding-cn",
        name: "Z.AI Coding CN",
        base_url: "https://open.bigmodel.cn/api/coding/paas/v4",
        auth_name: "Z.AI Coding CN API key",
        env_key: "ZAI_CODING_CN_API_KEY",
        api: ApiKind::OPENAI_COMPLETIONS,
        factory: zai_coding_cn::zai_coding_cn_provider,
    },
    FactorySpec {
        id: "moonshotai",
        name: "Moonshot AI",
        base_url: "https://api.moonshot.ai/v1",
        auth_name: "Moonshot AI API key",
        env_key: "MOONSHOT_API_KEY",
        api: ApiKind::OPENAI_COMPLETIONS,
        factory: moonshotai::moonshotai_provider,
    },
    FactorySpec {
        id: "moonshotai-cn",
        name: "Moonshot AI CN",
        base_url: "https://api.moonshot.cn/v1",
        auth_name: "Moonshot AI API key",
        env_key: "MOONSHOT_API_KEY",
        api: ApiKind::OPENAI_COMPLETIONS,
        factory: moonshotai_cn::moonshotai_cn_provider,
    },
    FactorySpec {
        id: "kimi-coding",
        name: "Kimi For Coding",
        base_url: "https://api.kimi.com/coding",
        auth_name: "Kimi API key",
        env_key: "KIMI_API_KEY",
        api: ApiKind::ANTHROPIC_MESSAGES,
        factory: kimi_coding::kimi_coding_provider,
    },
    FactorySpec {
        id: "meta",
        name: "Meta",
        base_url: "https://api.meta.ai/v1",
        auth_name: "Meta Model API key",
        env_key: "META_API_KEY",
        api: ApiKind::OPENAI_RESPONSES,
        factory: meta::meta_provider,
    },
    FactorySpec {
        id: "minimax",
        name: "MiniMax",
        base_url: "https://api.minimax.io/anthropic",
        auth_name: "MiniMax API key",
        env_key: "MINIMAX_API_KEY",
        api: ApiKind::ANTHROPIC_MESSAGES,
        factory: minimax::minimax_provider,
    },
    FactorySpec {
        id: "minimax-cn",
        name: "MiniMax CN",
        base_url: "https://api.minimaxi.com/anthropic",
        auth_name: "MiniMax CN API key",
        env_key: "MINIMAX_CN_API_KEY",
        api: ApiKind::ANTHROPIC_MESSAGES,
        factory: minimax_cn::minimax_cn_provider,
    },
    FactorySpec {
        id: "ant-ling",
        name: "Ant Ling",
        base_url: "https://api.ant-ling.com/v1",
        auth_name: "Ant Ling API key",
        env_key: "ANT_LING_API_KEY",
        api: ApiKind::OPENAI_COMPLETIONS,
        factory: ant_ling::ant_ling_provider,
    },
    FactorySpec {
        id: "qwen-token-plan",
        name: "Qwen Token Plan",
        base_url: "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        auth_name: "Qwen Token Plan API key",
        env_key: "QWEN_TOKEN_PLAN_API_KEY",
        api: ApiKind::OPENAI_COMPLETIONS,
        factory: qwen_token_plan::qwen_token_plan_provider,
    },
    FactorySpec {
        id: "qwen-token-plan-cn",
        name: "Qwen Token Plan CN",
        base_url: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
        auth_name: "Qwen Token Plan CN API key",
        env_key: "QWEN_TOKEN_PLAN_CN_API_KEY",
        api: ApiKind::OPENAI_COMPLETIONS,
        factory: qwen_token_plan_cn::qwen_token_plan_cn_provider,
    },
];

#[test]
fn factory_shape_matches_upstream() {
    assert_eq!(SPECS.len(), 11);
    for spec in SPECS {
        let provider = (spec.factory)();
        assert_eq!(provider.id(), spec.id);
        assert_eq!(provider.name(), spec.name);
        assert_eq!(provider.base_url(), Some(spec.base_url));
        // No factory in this group sets default headers upstream.
        assert!(provider.headers().is_none(), "{} headers", spec.id);

        let models = provider.get_models();
        assert!(!models.is_empty(), "{} has no catalog models", spec.id);
        for model in &models {
            assert_eq!(model.provider, spec.id);
            assert_eq!(model.api.as_str(), spec.api, "{}/{}", spec.id, model.id);
        }
    }
}

#[tokio::test]
async fn auth_resolves_the_env_key() {
    for spec in SPECS {
        let provider = (spec.factory)();
        let auth = provider
            .auth()
            .api_key
            .as_ref()
            .unwrap_or_else(|| panic!("{} api key auth", spec.id));
        assert_eq!(auth.name(), spec.auth_name);

        let ctx = MapAuthContext(HashMap::from([(
            spec.env_key.to_owned(),
            "test-key".to_owned(),
        )]));
        let result = auth
            .resolve(&ctx, None)
            .await
            .expect("resolve")
            .unwrap_or_else(|| panic!("{} not configured", spec.id));
        assert_eq!(result.auth.api_key.as_deref(), Some("test-key"));
        assert_eq!(result.source.as_deref(), Some(spec.env_key));

        let empty = MapAuthContext(HashMap::new());
        assert!(auth.resolve(&empty, None).await.expect("resolve").is_none());
    }
}

/// Upstream `openai-completions-tool-choice.test.ts`: `zaiToolStream` is set
/// on all current GLM models — for both Z.AI regions. The older GLM 4.5
/// family has been retired upstream (catalog refresh @ 4181f66).
#[test]
fn zai_tool_stream_compat_is_baked_into_the_catalog() {
    // zai keeps the GLM-4.7→GLM-5.2 family; zai-coding-cn shrank to the
    // GLM-5.3 family + glm-4.6v in the 2026-09-20 models.dev snapshot
    // (rules unchanged — same G2 drift class as the V14-09 xiaomi split).
    type FactoryIds<'a> = (fn() -> Arc<dyn Provider>, &'a [&'a str]);
    let cases: Vec<FactoryIds> = vec![
        (
            zai::zai_provider as fn() -> Arc<dyn Provider>,
            &["glm-4.7", "glm-5-turbo", "glm-5.2", "glm-5.2-highspeed"],
        ),
        (
            zai_coding_cn::zai_coding_cn_provider,
            &["glm-5.3", "glm-5.3-flash", "glm-5.3-highspeed"],
        ),
    ];
    for (factory, ids) in cases {
        let provider = factory();
        let models = provider.get_models();
        let zai_tool_stream = |id: &str| {
            models
                .iter()
                .find(|model| model.id == id)
                .unwrap_or_else(|| panic!("{id} missing"))
                .compat
                .as_ref()
                .and_then(|compat| compat.zai_tool_stream)
        };
        for id in ids {
            assert_eq!(zai_tool_stream(id), Some(true), "{id}");
        }
    }
}

/// Upstream `providers.test.ts` "uses official Kimi K3 pricing for Moonshot
/// providers"; the catalog carries the post-#9548 Kimi deferred-tools face
/// (`supportsMidConvoSystemMessages` + `supportsMidConvoToolAdditions`,
/// 9e05370b2 — `deferredToolsMode` left the catalog).
#[test]
fn moonshotai_kimi_k3_pricing_and_mid_convo_compat() {
    for factory in [
        moonshotai::moonshotai_provider as fn() -> Arc<dyn Provider>,
        moonshotai_cn::moonshotai_cn_provider,
    ] {
        let provider = factory();
        let model = provider
            .get_models()
            .into_iter()
            .find(|model| model.id == "kimi-k3")
            .expect("kimi-k3");
        assert_eq!(model.cost.rates.input, 3.0);
        assert_eq!(model.cost.rates.output, 15.0);
        assert_eq!(model.cost.rates.cache_read, 0.3);
        // v1.1.0 `a2eef9eb6`: models.dev lists Kimi K3's input rate as its
        // cache-write cost on Moonshot, which does not bill cache writes; the
        // generator now pins the whole Kimi K3 cost to `KIMI_K3_COST`
        // (cacheWrite 0) instead of passing the source value through.
        assert_eq!(model.cost.rates.cache_write, 0.0);
        // Post-#9548 catalog shape: `deferredToolsMode` was replaced by the
        // mid-conversation compat faces (9e05370b2; wire convergence V15-06,
        // T-V15-02-1).
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|compat| compat.supports_mid_convo_system_messages),
            Some(true)
        );
        assert_eq!(
            model
                .compat
                .as_ref()
                .and_then(|compat| compat.supports_mid_convo_tool_additions),
            Some(true)
        );
    }
}

/// Upstream `providers.test.ts` "uses API-equivalent implied pricing for Kimi
/// Coding subscription models".
#[test]
fn kimi_coding_implied_pricing() {
    let provider = kimi_coding::kimi_coding_provider();
    let models = provider.get_models();
    let cost = |id: &str| {
        models
            .iter()
            .find(|model| model.id == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .cost
            .rates
            .clone()
    };
    let k3 = cost("k3");
    assert_eq!(
        (k3.input, k3.output, k3.cache_read, k3.cache_write),
        (3.0, 15.0, 0.3, 0.0)
    );
    let highspeed = cost("kimi-for-coding-highspeed");
    assert_eq!(
        (
            highspeed.input,
            highspeed.output,
            highspeed.cache_read,
            highspeed.cache_write
        ),
        (1.9, 8.0, 0.38, 0.0)
    );
}

/// `kimi-coding` OAuth ("Kimi Code (subscription)", upstream
/// `lazyOAuth`-wired device-code flow) is wired into the factory slot
/// (T13 W5, deviation D-029 closed).
#[test]
fn kimi_coding_oauth_slot_wired() {
    let provider = kimi_coding::kimi_coding_provider();
    assert!(provider.auth().api_key.is_some());
    let oauth = provider.auth().oauth.as_ref().expect("oauth");
    assert_eq!(oauth.name(), "Kimi Code (subscription)");
}

/// Upstream `qwen-token-plan-models.test.ts`: both regions expose all text
/// models and omit the image-generation models.
#[test]
fn qwen_token_plan_exposes_text_models_only() {
    const TEXT_MODELS: [&str; 16] = [
        "MiniMax-M2.5",
        "deepseek-v3.2",
        "deepseek-v4-flash",
        "deepseek-v4-flash-0731",
        "deepseek-v4-pro",
        "glm-5",
        "glm-5.1",
        "glm-5.2",
        "kimi-k2.5",
        "kimi-k2.6",
        "kimi-k2.7-code",
        "qwen3.6-flash",
        "qwen3.6-plus",
        "qwen3.7-max",
        "qwen3.7-plus",
        "qwen3.8-max",
    ];
    const IMAGE_MODELS: [&str; 4] = [
        "qwen-image-2.0",
        "qwen-image-2.0-pro",
        "wan2.7-image",
        "wan2.7-image-pro",
    ];
    for factory in [
        qwen_token_plan::qwen_token_plan_provider as fn() -> Arc<dyn Provider>,
        qwen_token_plan_cn::qwen_token_plan_cn_provider,
    ] {
        let provider = factory();
        let models = provider.get_models();
        let ids: Vec<&str> = models.iter().map(|model| model.id.as_str()).collect();
        for expected in TEXT_MODELS {
            assert!(
                ids.contains(&expected),
                "{} missing {expected}",
                provider.id()
            );
        }
        for excluded in IMAGE_MODELS {
            assert!(
                !ids.contains(&excluded),
                "{} should not include {excluded}",
                provider.id()
            );
        }
    }
}

/// The MiniMax pair speaks `anthropic-messages` against the provider's
/// `/anthropic` endpoint; every catalog model inherits the factory base URL.
#[test]
fn minimax_models_use_anthropic_messages_on_the_factory_base_url() {
    for factory in [
        minimax::minimax_provider as fn() -> Arc<dyn Provider>,
        minimax_cn::minimax_cn_provider,
    ] {
        let provider = factory();
        for model in provider.get_models() {
            assert_eq!(model.api.as_str(), ApiKind::ANTHROPIC_MESSAGES);
            assert_eq!(model.base_url, provider.base_url().expect("base url"));
        }
    }
}
