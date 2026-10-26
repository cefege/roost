//! Compat detection matrix hit tests (T13 W4).
//!
//! Covers the full `detectCompat` provider/baseUrl matrix of upstream
//! `packages/ai/src/api/openai-completions.ts` (zai / zai-coding-cn / together /
//! moonshotai(+-cn) / openrouter / cloudflare-workers-ai / cloudflare-ai-gateway /
//! nvidia / ant-ling / cerebras / xai / chutes / deepseek / opencode) plus the
//! compat deltas baked into the vendored catalog by `generate-models.ts`
//! (zaiToolStream, Kimi deferredToolsMode, OpenAI grammar tools, OpenRouter
//! cacheControlFormat, per-model thinkingLevelMap, …).

use rpi_ai::api::anthropic_messages::get_anthropic_compat;
use rpi_ai::api::openai_completions::{ResolvedOpenAICompletionsCompat, detect_compat, get_compat};
use rpi_ai::generated::{get_builtin_model, get_builtin_models};
use rpi_ai::models::get_supported_thinking_levels;
use rpi_ai::types::{
    CacheControlFormat, MaxTokensField, Model, ModelThinkingLevel, SessionAffinityFormat,
    ThinkingFormat,
};
use serde_json::json;

fn make_model(provider: &str, base_url: &str, id: &str) -> Model {
    serde_json::from_value(json!({
        "id": id, "name": id, "api": "openai-completions", "provider": provider,
        "baseUrl": base_url, "reasoning": false, "input": ["text"],
        "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
        "contextWindow": 1000, "maxTokens": 100
    }))
    .expect("model")
}

/// The fully standard (OpenAI first-party) detection result: every matrix
/// case below is expressed as mutations against this baseline.
fn standard() -> ResolvedOpenAICompletionsCompat {
    detect_compat(&make_model("openai", "https://api.openai.com/v1", "gpt-5"))
}

#[test]
fn test_detect_compat_standard_openai_baseline() {
    let compat = standard();
    assert!(compat.supports_store);
    assert!(compat.supports_developer_role);
    assert!(compat.supports_reasoning_effort);
    assert!(compat.supports_usage_in_streaming);
    // 2c3041242: finish_reason streams are the default; providers opt out via
    // `compat.supportsFinishReason: false`.
    assert!(compat.supports_finish_reason);
    assert_eq!(compat.max_tokens_field, MaxTokensField::MaxCompletionTokens);
    assert!(!compat.requires_tool_result_name);
    assert!(!compat.requires_assistant_after_tool_result);
    assert!(!compat.requires_thinking_as_text);
    assert!(!compat.requires_reasoning_content_on_assistant_messages);
    assert_eq!(compat.thinking_format, ThinkingFormat::Openai);
    assert!(!compat.zai_tool_stream);
    // #9816 (890f92088): OpenAI compatibility alone does not imply strict
    // support — even the first-party detection defaults to non-strict;
    // capable models carry explicit catalog metadata instead. G2: was
    // `assert!(compat.supports_strict_mode)` before the #9816 flip.
    assert!(!compat.supports_strict_mode);
    assert!(!compat.supports_open_ai_grammar_tools);
    assert_eq!(compat.cache_control_format, None);
    assert!(!compat.send_session_affinity_headers);
    // #9548: Kimi face moved to supportsMidConvo* (default false).
    assert!(!compat.supports_mid_convo_system_messages);
    assert!(!compat.supports_mid_convo_tool_additions);
    assert_eq!(
        compat.session_affinity_format,
        SessionAffinityFormat::Openai
    );
    assert!(compat.supports_long_cache_retention);
}

#[test]
fn test_detect_compat_zai_hits() {
    let mut expected = standard();
    expected.supports_store = false;
    expected.supports_developer_role = false;
    expected.supports_reasoning_effort = false;
    expected.thinking_format = ThinkingFormat::Zai;
    // 2fe21b407 (#7174): Z.AI ignores `max_completion_tokens` — the runtime
    // detection now sends `max_tokens` (T20 Wave C; was MaxCompletionTokens).
    expected.max_tokens_field = MaxTokensField::MaxTokens;

    // Provider-id hits.
    for provider in ["zai", "zai-coding-cn"] {
        let model = make_model(provider, "https://example.com/v1", "glm-4.7");
        assert_eq!(detect_compat(&model), expected, "provider {provider}");
    }
    // baseUrl-only hits (custom provider id).
    for base_url in [
        "https://api.z.ai/api/coding/paas/v4",
        "https://open.bigmodel.cn/api/paas/v4",
    ] {
        let model = make_model("custom", base_url, "glm-4.7");
        assert_eq!(detect_compat(&model), expected, "baseUrl {base_url}");
    }
}

#[test]
fn test_detect_compat_together_hits() {
    let mut expected = standard();
    expected.supports_store = false;
    expected.supports_developer_role = false;
    expected.supports_reasoning_effort = false;
    expected.max_tokens_field = MaxTokensField::MaxTokens;
    expected.thinking_format = ThinkingFormat::Together;
    expected.supports_strict_mode = false;
    expected.supports_long_cache_retention = false;

    let model = make_model("together", "https://api.together.ai/v1", "meta-llama/x");
    assert_eq!(detect_compat(&model), expected);
    // baseUrl-only hits: both together.ai and the legacy together.xyz.
    for base_url in ["https://api.together.ai/v1", "https://api.together.xyz/v1"] {
        let model = make_model("custom", base_url, "meta-llama/x");
        assert_eq!(detect_compat(&model), expected, "baseUrl {base_url}");
    }
}

#[test]
fn test_detect_compat_moonshot_hits() {
    let mut expected = standard();
    expected.supports_store = false;
    expected.supports_developer_role = false;
    expected.supports_reasoning_effort = false;
    expected.max_tokens_field = MaxTokensField::MaxTokens;
    expected.supports_strict_mode = false;

    for provider in ["moonshotai", "moonshotai-cn"] {
        let model = make_model(provider, "https://example.com/v1", "kimi-k2");
        assert_eq!(detect_compat(&model), expected, "provider {provider}");
    }
    // baseUrl-only hit (api.moonshot. prefix covers .ai and .cn).
    let model = make_model("custom", "https://api.moonshot.cn/v1", "kimi-k2");
    assert_eq!(detect_compat(&model), expected);
}

#[test]
fn test_detect_compat_openrouter_hits() {
    let mut expected = standard();
    expected.supports_developer_role = false;
    expected.thinking_format = ThinkingFormat::Openrouter;
    expected.session_affinity_format = SessionAffinityFormat::Openrouter;
    // #9102 (bbb61e34a): OpenRouter derives session affinity headers by
    // default (`x-session-id`); explicit compat can opt out.
    expected.send_session_affinity_headers = true;

    // Generic OpenRouter model: no developer role.
    let model = make_model("openrouter", "https://openrouter.ai/api/v1", "meta-llama/x");
    assert_eq!(detect_compat(&model), expected);

    // anthropic/* and openai/* ids get the developer role back.
    let mut dev_expected = expected.clone();
    dev_expected.supports_developer_role = true;
    let model = make_model("openrouter", "https://openrouter.ai/api/v1", "openai/gpt-5");
    assert_eq!(detect_compat(&model), dev_expected);

    // anthropic/* ids additionally select Anthropic cache-control markers.
    let mut anthropic_expected = dev_expected;
    anthropic_expected.cache_control_format = Some(CacheControlFormat::Anthropic);
    let model = make_model(
        "openrouter",
        "https://openrouter.ai/api/v1",
        "anthropic/claude-sonnet-4.6",
    );
    assert_eq!(detect_compat(&model), anthropic_expected);

    // baseUrl-only hit behaves like the provider-id hit.
    let model = make_model("custom", "https://openrouter.ai/api/v1", "meta-llama/x");
    assert_eq!(detect_compat(&model), expected);
}

#[test]
fn test_detect_compat_cloudflare_hits() {
    // Workers AI: store/developer off, long cache retention off; reasoning
    // effort and max_completion_tokens stay standard (strict follows the
    // #9816 non-strict runtime default like every endpoint).
    let mut workers = standard();
    workers.supports_store = false;
    workers.supports_developer_role = false;
    workers.supports_long_cache_retention = false;
    let model = make_model(
        "cloudflare-workers-ai",
        "https://api.cloudflare.com/client/v4/accounts/x/ai/v1",
        "@cf/meta/llama",
    );
    assert_eq!(detect_compat(&model), workers);
    let model = make_model("custom", "https://api.cloudflare.com/client/v4", "@cf/x");
    assert_eq!(detect_compat(&model), workers);

    // AI Gateway: additionally no reasoning effort, max_tokens, no strict.
    let mut gateway = workers.clone();
    gateway.supports_reasoning_effort = false;
    gateway.max_tokens_field = MaxTokensField::MaxTokens;
    gateway.supports_strict_mode = false;
    let model = make_model(
        "cloudflare-ai-gateway",
        "https://gateway.ai.cloudflare.com/v1/x/y/openai",
        "gpt-5",
    );
    assert_eq!(detect_compat(&model), gateway);
    let model = make_model("custom", "https://gateway.ai.cloudflare.com/v1/x", "gpt-5");
    assert_eq!(detect_compat(&model), gateway);
}

#[test]
fn test_detect_compat_nvidia_ant_ling_hits() {
    let mut nvidia = standard();
    nvidia.supports_store = false;
    nvidia.supports_developer_role = false;
    nvidia.supports_reasoning_effort = false;
    nvidia.max_tokens_field = MaxTokensField::MaxTokens;
    nvidia.supports_strict_mode = false;
    nvidia.supports_long_cache_retention = false;
    let model = make_model("nvidia", "https://integrate.api.nvidia.com/v1", "nvidia/x");
    assert_eq!(detect_compat(&model), nvidia);
    let model = make_model("custom", "https://integrate.api.nvidia.com/v1", "x");
    assert_eq!(detect_compat(&model), nvidia);

    let mut ant_ling = nvidia.clone();
    // G2 (#9816): ant-ling strict was a runtime `= true` grant before the
    // flip; like every openai-completions endpoint it now defaults to
    // non-strict at detection time (upstream pins it via catalog metadata).
    ant_ling.thinking_format = ThinkingFormat::AntLing;
    let model = make_model("ant-ling", "https://api.ant-ling.com/v1", "ring-1");
    assert_eq!(detect_compat(&model), ant_ling);
    let model = make_model("custom", "https://api.ant-ling.com/v1", "ring-1");
    assert_eq!(detect_compat(&model), ant_ling);
}

#[test]
fn test_detect_compat_cerebras_xai_chutes_deepseek_opencode_hits() {
    // Cerebras: store/developer role flip; #9804 (af7359b90): mixed
    // strict/unstrict tool usage 400s, so strict mode is never declared.
    let mut cerebras = standard();
    cerebras.supports_store = false;
    cerebras.supports_developer_role = false;
    cerebras.supports_strict_mode = false;
    let model = make_model("cerebras", "https://api.cerebras.ai/v1", "llama-4");
    assert_eq!(detect_compat(&model), cerebras);
    let model = make_model("custom", "https://api.cerebras.ai/v1", "llama-4");
    assert_eq!(detect_compat(&model), cerebras);

    // xAI (Grok): additionally no reasoning effort. G2 (#9816): strict was
    // a runtime `= true` grant before the flip (metadata carries it now).
    let mut xai = cerebras.clone();
    xai.supports_reasoning_effort = false;
    let model = make_model("xai", "https://api.x.ai/v1", "grok-4");
    assert_eq!(detect_compat(&model), xai);
    let model = make_model("custom", "https://api.x.ai/v1", "grok-4");
    assert_eq!(detect_compat(&model), xai);

    // Chutes (baseUrl-only hit): max_tokens instead of max_completion_tokens.
    // G2 (#9816): strict was a runtime `= true` grant before the flip.
    let mut chutes = cerebras.clone();
    chutes.max_tokens_field = MaxTokensField::MaxTokens;
    let model = make_model("custom", "https://llm.chutes.ai/v1", "deepseek-x");
    assert_eq!(detect_compat(&model), chutes);

    // DeepSeek: deepseek thinking format + reasoning_content replay.
    // G2 (#9816): strict was a runtime `= true` grant before the flip.
    let mut deepseek = cerebras.clone();
    deepseek.thinking_format = ThinkingFormat::Deepseek;
    deepseek.requires_reasoning_content_on_assistant_messages = true;
    // c185d4123: DeepSeek APIs take `max_tokens` (T20 Wave C; was
    // MaxCompletionTokens).
    deepseek.max_tokens_field = MaxTokensField::MaxTokens;
    let model = make_model("deepseek", "https://api.deepseek.com/v1", "deepseek-chat");
    assert_eq!(detect_compat(&model), deepseek);
    let model = make_model("custom", "https://api.deepseek.com/v1", "deepseek-chat");
    assert_eq!(detect_compat(&model), deepseek);

    // opencode provider id, and opencode-go via its opencode.ai baseUrl.
    // G2 (#9816): strict was a runtime `= true` grant before the flip.
    let opencode = cerebras.clone();
    let model = make_model("opencode", "https://opencode.ai/zen/v1", "grok-build-0.1");
    assert_eq!(detect_compat(&model), opencode);
    let model = make_model("opencode-go", "https://opencode.ai/go/v1", "glm-5.2");
    assert_eq!(detect_compat(&model), opencode);
}

/// #9102 (bbb61e34a): explicit `sendSessionAffinityHeaders: false` opts out
/// of the OpenRouter default (upstream
/// `openai-completions-prompt-cache.test.ts` "allows OpenRouter session
/// headers to be disabled").
#[test]
fn test_detect_compat_openrouter_session_affinity_opt_out() {
    let model = serde_json::from_value(json!({
        "id": "meta/llama", "name": "m", "api": "openai-completions", "provider": "openrouter",
        "baseUrl": "https://openrouter.ai/api/v1", "reasoning": false, "input": ["text"],
        "cost": {"input": 1.0, "output": 1.0, "cacheRead": 0.1, "cacheWrite": 1.0},
        "contextWindow": 1000, "maxTokens": 100,
        "compat": {"sendSessionAffinityHeaders": false}
    }))
    .expect("model");
    let compat = get_compat(&model);
    assert!(!compat.send_session_affinity_headers);
    assert_eq!(
        compat.session_affinity_format,
        SessionAffinityFormat::Openrouter
    );
}

/// #9804 (af7359b90, FR-J): the strict-mode exclusion also holds when
/// Cerebras is reached via a `cerebras.ai` baseUrl on a custom provider id
/// (upstream `cache-retention.test.ts` #9804 regression ports live in
/// `openai_completions.rs` inline tests — payload-level assertions).
#[test]
fn test_detect_compat_cerebras_strict_mode_exclusion() {
    for (provider, base_url) in [
        ("cerebras", "https://example.com/v1"),
        ("custom", "https://api.cerebras.ai/v1"),
    ] {
        let model = make_model(provider, base_url, "gpt-oss-120b");
        let compat = detect_compat(&model);
        assert!(!compat.supports_strict_mode, "{provider}/{base_url}");
        assert!(!compat.supports_store);
    }
}

// ---------------------------------------------------------------------------
// Compat deltas baked into the vendored catalog by generate-models.ts
// ---------------------------------------------------------------------------

#[test]
fn test_catalog_zai_tool_stream_baked() {
    let glm47 = get_builtin_model("zai", "glm-4.7").expect("glm-4.7");
    let compat = glm47.compat.as_ref().expect("compat");
    assert_eq!(compat.zai_tool_stream, Some(true));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Zai));
    // Merged over detection: zaiToolStream survives get_compat.
    assert!(get_compat(glm47).zai_tool_stream);

    // Older GLM 4.5 family has been retired upstream; all remaining zai
    // models (glm-4.7, glm-5-turbo, glm-5.2, glm-5.2-highspeed) support
    // tool streaming. Verify glm-5-turbo as a second positive case.
    let glm5t = get_builtin_model("zai", "glm-5-turbo").expect("glm-5-turbo");
    assert!(get_compat(glm5t).zai_tool_stream);

    // GLM 5.2 thinking level map. Since 3de00332f ("derive Z.AI reasoning
    // effort metadata") the map comes from models.dev effort options with
    // glm-5.2 keeping its `off: "none"` override; `low` is no longer forced
    // to "high" (that hardcoded map was 75b0d723c, replaced upstream).
    let glm52 = get_builtin_model("zai", "glm-5.2").expect("glm-5.2");
    let map = glm52.thinking_level_map.as_ref().expect("thinkingLevelMap");
    assert_eq!(
        map.get(&ModelThinkingLevel::Off),
        Some(&Some("none".to_owned()))
    );
    assert_eq!(map.get(&ModelThinkingLevel::Minimal), Some(&None));
    assert_eq!(map.get(&ModelThinkingLevel::Low), Some(&None));
    assert_eq!(
        map.get(&ModelThinkingLevel::High),
        Some(&Some("high".to_owned()))
    );
    assert_eq!(
        map.get(&ModelThinkingLevel::Max),
        Some(&Some("max".to_owned()))
    );
}

#[test]
fn test_catalog_kimi_mid_convo_tool_additions() {
    let k3 = get_builtin_model("moonshotai", "kimi-k3").expect("kimi-k3");
    let compat = k3.compat.as_ref().expect("compat");
    // Post-#9548 catalog shape (9e05370b2): the Kimi deferred-tools face is
    // carried as mid-conversation system/tool-addition support instead of
    // `deferredToolsMode` (which left the catalog AND the resolved compat —
    // detection defaults to false; the catalog carries the flags
    // explicitly). Wire-side convergence: V15-06 (T-V15-02-1).
    assert_eq!(compat.supports_mid_convo_system_messages, Some(true));
    assert_eq!(compat.supports_mid_convo_tool_additions, Some(true));
    assert_eq!(
        compat.requires_reasoning_content_on_assistant_messages,
        Some(true)
    );
    assert_eq!(compat.max_tokens_field, Some(MaxTokensField::MaxTokens));

    let k3_cn = get_builtin_model("moonshotai-cn", "kimi-k3").expect("kimi-k3 cn");
    assert_eq!(
        k3_cn
            .compat
            .as_ref()
            .and_then(|c| c.supports_mid_convo_system_messages),
        Some(true)
    );
    assert_eq!(
        k3_cn
            .compat
            .as_ref()
            .and_then(|c| c.supports_mid_convo_tool_additions),
        Some(true)
    );
}

#[test]
fn test_catalog_openai_grammar_tools_baked() {
    // GPT-5 family on first-party Responses APIs passes Lark/regex grammar
    // tools through.
    let gpt5 = get_builtin_model("openai", "gpt-5").expect("gpt-5");
    let compat = gpt5.compat.as_ref().expect("compat");
    assert_eq!(compat.supports_open_ai_grammar_tools, Some(true));
    assert_eq!(compat.supports_strict_mode, Some(true));

    // GPT-5.4 retired upstream (2e6fe2f98); the grammar-tool assertion moved
    // to gpt-5.5 with it.
    let codex = get_builtin_model("openai-codex", "gpt-5.5").expect("codex gpt-5.5");
    let compat = codex.compat.as_ref().expect("compat");
    assert_eq!(compat.supports_open_ai_grammar_tools, Some(true));
    assert_eq!(compat.supports_tool_search, Some(true));

    // Pre-GPT-5 models are excluded (OpenAI rejects custom tools there).
    let gpt4o = get_builtin_model("openai", "gpt-4o").expect("gpt-4o");
    assert_eq!(
        gpt4o
            .compat
            .as_ref()
            .and_then(|c| c.supports_open_ai_grammar_tools),
        None
    );
}

#[test]
fn test_catalog_anthropic_compat_baked() {
    let fable = get_builtin_model("anthropic", "claude-fable-5").expect("fable-5");
    let compat = fable.compat.as_ref().expect("compat");
    assert_eq!(compat.force_adaptive_thinking, Some(true));
    assert_eq!(compat.supports_strict_tools, Some(true));
    let map = fable.thinking_level_map.as_ref().expect("thinkingLevelMap");
    assert_eq!(map.get(&ModelThinkingLevel::Off), Some(&None));
    assert_eq!(
        map.get(&ModelThinkingLevel::Xhigh),
        Some(&Some("xhigh".to_owned()))
    );
}

#[test]
fn test_catalog_github_copilot_eager_streaming_baked() {
    // Copilot's Claude Haiku 4.5 rejects eager tool input streaming. Sonnet 4 /
    // 4.5 were in the unsupported set as well (rule list intact at
    // generate-models.ts:268-272) but models.dev has retired them from the
    // Copilot catalog, so the regenerated data only carries Haiku 4.5;
    // upstream ef41a24b3 derives catalog-sensitive ids dynamically for the
    // same reason. G2: old expectation iterated all three ids.
    let model = get_builtin_model("github-copilot", "claude-haiku-4.5").expect("claude-haiku-4.5");
    assert_eq!(
        model
            .compat
            .as_ref()
            .and_then(|c| c.supports_eager_tool_input_streaming),
        Some(false)
    );
    assert!(!get_anthropic_compat(model).supports_eager_tool_input_streaming);
}

#[test]
fn test_catalog_together_reasoning_variants_baked() {
    // Reasoning-effort models: effort on, openai thinking format.
    // G2 (2026-10-08 catalog refresh): the previous `openai/gpt-oss-20b` /
    // `deepseek-ai/DeepSeek-V4-Pro` ids were retired by the snapshot; the
    // same baked rules moved to their current siblings.
    let gpt_oss = get_builtin_model("together", "openai/gpt-oss-120b").expect("gpt-oss-120b");
    let compat = gpt_oss.compat.as_ref().expect("compat");
    assert_eq!(compat.supports_reasoning_effort, Some(true));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Openai));
    assert!(get_compat(gpt_oss).supports_reasoning_effort);
    let map = gpt_oss.thinking_level_map.as_ref().expect("map");
    assert_eq!(map.get(&ModelThinkingLevel::Off), Some(&None));

    // Default reasoning models keep the together toggle format with effort off.
    let r1 =
        get_builtin_model("together", "deepseek-ai/DeepSeek-V4-Pro-0813").expect("ds-v4-pro-0813");
    let compat = r1.compat.as_ref().expect("compat");
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Together));
    assert_eq!(compat.supports_long_cache_retention, Some(false));
}

#[test]
fn test_catalog_openrouter_cache_control_baked() {
    // "~anthropic/..." ids bypass runtime detection (detect keys on an
    // "anthropic/" prefix); the generator bakes cacheControlFormat instead.
    let tilde = get_builtin_model("openrouter", "~anthropic/claude-fable-latest").expect("tilde");
    assert_eq!(
        tilde.compat.as_ref().and_then(|c| c.cache_control_format),
        Some(CacheControlFormat::Anthropic)
    );
    assert_eq!(
        get_compat(tilde).cache_control_format,
        Some(CacheControlFormat::Anthropic)
    );

    // Plain "anthropic/..." ids resolve through detection as well.
    // (2026-10-01 v6 snapshot: `claude-3-haiku` left the OpenRouter
    // catalog; the current Claude line id covers the same detection face.)
    let plain = get_builtin_model("openrouter", "anthropic/claude-fable-5").expect("plain");
    assert_eq!(
        get_compat(plain).cache_control_format,
        Some(CacheControlFormat::Anthropic)
    );
}

// ---------------------------------------------------------------------------
// Routing preferences: full-field serde shape
// ---------------------------------------------------------------------------

#[test]
fn test_openrouter_routing_full_field_roundtrip() {
    // OpenRouterRouting contents are snake_case on the wire (upstream
    // types.ts; the object is sent verbatim as the `provider` request field).
    let value = json!({
        "allow_fallbacks": false,
        "require_parameters": true,
        "data_collection": "deny",
        "zdr": true,
        "enforce_distillable_text": true,
        "order": ["Anthropic", "OpenAI"],
        "only": ["Anthropic"],
        "ignore": ["Azure"],
        "quantizations": ["fp8"],
        "sort": {"by": "price", "partition": "model"},
        "max_price": {"prompt": 5.0, "completion": "10", "image": 1.0, "audio": 2.0, "request": "0.5"},
        "preferred_min_throughput": {"p50": 30.0},
        "preferred_max_latency": 2.5
    });
    let routing: rpi_ai::types::OpenRouterRouting =
        serde_json::from_value(value.clone()).expect("routing");
    assert_eq!(serde_json::to_value(&routing).expect("json"), value);

    // String form of `sort` round-trips too.
    let routing: rpi_ai::types::OpenRouterRouting =
        serde_json::from_value(json!({"sort": "price"})).expect("routing");
    assert_eq!(
        serde_json::to_value(&routing).expect("json"),
        json!({"sort": "price"})
    );
}

#[test]
fn test_grammar_tool_variants_lark_and_regex() {
    // Both OpenAI grammar variants (lark/regex) survive the compat flag flow.
    let value = json!({"type": "grammar", "variants": {"openai_lark": "start: x", "openai_regex": "[a-z]+"}});
    let config: rpi_ai::types::ConstrainedSamplingConfig =
        serde_json::from_value(value.clone()).expect("config");
    assert_eq!(serde_json::to_value(&config).expect("json"), value);
}

// ---------------------------------------------------------------------------
// R2.1.1/R2.1.2 (V16-01 FR-A): capability follows catalog metadata across the
// model-cluster access faces — upstream b7f4b05c7 (Opus effort levels) /
// db91e0331 (GPT-6 Sol·Luna) / 27c072e98 (Copilot family). rpi adds no
// hardcoded family table; these tests pin the metadata derivation over the
// whole access-face matrix (V15-02 FR-G "capability follows metadata" form).
// Data entries for the new families land with the V16-02 catalog regen and
// are covered automatically — model ids here are computed, never literal.
// ---------------------------------------------------------------------------

/// Spec mirror of `getSupportedThinkingLevels` (models.ts / models.rs):
/// non-reasoning models expose `off` only; a level is available iff its
/// `thinkingLevelMap` entry maps to a value; JSON `null` disables it; absent
/// entries keep the default ladder except xhigh/max, which always require an
/// explicit mapping. Mirrored independently so that any hardcoded family
/// table added to level resolution breaks the equality below.
fn metadata_derived_levels(model: &Model) -> Vec<ModelThinkingLevel> {
    let ladder = [
        ModelThinkingLevel::Off,
        ModelThinkingLevel::Minimal,
        ModelThinkingLevel::Low,
        ModelThinkingLevel::Medium,
        ModelThinkingLevel::High,
        ModelThinkingLevel::Xhigh,
        ModelThinkingLevel::Max,
    ];
    if !model.reasoning {
        return vec![ModelThinkingLevel::Off];
    }
    ladder
        .iter()
        .copied()
        .filter(|level| {
            match model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(level))
            {
                Some(None) => false,
                Some(Some(_)) => true,
                None => !matches!(level, ModelThinkingLevel::Xhigh | ModelThinkingLevel::Max),
            }
        })
        .collect()
}

/// R2.1.2 inheritance matrix, leg 1: every model exposed through the three
/// GPT access faces (OpenAI API key / Codex subscription / GitHub Copilot)
/// resolves its thinking levels purely from its own catalog metadata.
#[test]
fn test_access_face_levels_follow_catalog_metadata() {
    for provider in ["openai", "openai-codex", "github-copilot"] {
        let models = get_builtin_models(provider);
        assert!(!models.is_empty(), "{provider} catalog face is empty");
        for model in models {
            assert_eq!(
                get_supported_thinking_levels(model),
                metadata_derived_levels(model),
                "{provider}/{}: supported levels diverge from thinkingLevelMap",
                model.id
            );
        }
    }
}

/// R2.1.2 inheritance matrix, leg 2: model ids shared by the three GPT
/// access faces resolve on every face as reasoning models with metadata-
/// derived levels (db91e0331 "includes official metadata for OpenAI and
/// Codex" / 27c072e98 Copilot counterparts — the shared set grows with the
/// V16-02 regen without touching this test).
#[test]
fn test_three_access_faces_share_capability_metadata() {
    let faces: [Vec<&str>; 3] = [
        get_builtin_models("openai")
            .iter()
            .map(|m| m.id.as_str())
            .collect(),
        get_builtin_models("openai-codex")
            .iter()
            .map(|m| m.id.as_str())
            .collect(),
        get_builtin_models("github-copilot")
            .iter()
            .map(|m| m.id.as_str())
            .collect(),
    ];
    let shared: Vec<&str> = faces[0]
        .iter()
        .filter(|id| faces[1].contains(id) && faces[2].contains(id))
        .copied()
        .collect();
    assert!(
        !shared.is_empty(),
        "three-face intersection must not be empty"
    );

    for id in shared {
        for provider in ["openai", "openai-codex", "github-copilot"] {
            let model = get_builtin_model(provider, id)
                .unwrap_or_else(|| panic!("{provider}/{id} missing from catalog"));
            assert!(model.reasoning, "{provider}/{id} must be a reasoning model");
            assert_eq!(
                get_supported_thinking_levels(model),
                metadata_derived_levels(model),
                "{provider}/{id}: levels diverge from thinkingLevelMap"
            );
        }
    }
}

/// R2.1.1/R2.1.2 inheritance matrix, leg 3: Claude models shared between
/// direct Anthropic and the GitHub Copilot face (upstream
/// github-copilot-anthropic.test.ts walks the same pair) — capabilities come
/// from each face's own metadata; the Copilot effort ladder for the Opus 5.5
/// generation arrives with the V16-02 regen (b7f4b05c7's five-level map).
#[test]
fn test_copilot_claude_capabilities_follow_catalog_metadata() {
    let anthropic_ids: Vec<String> = get_builtin_models("anthropic")
        .iter()
        .map(|m| m.id.clone())
        .collect();
    let copilot_models: Vec<&Model> = get_builtin_models("github-copilot")
        .iter()
        .filter(|m| m.api.as_str() == "anthropic-messages")
        .collect();
    assert!(
        !copilot_models.is_empty(),
        "copilot anthropic face is empty"
    );

    for model in copilot_models {
        assert_eq!(
            get_supported_thinking_levels(model),
            metadata_derived_levels(model),
            "github-copilot/{}: levels diverge from thinkingLevelMap",
            model.id
        );
        if anthropic_ids.contains(&model.id) {
            let direct = get_builtin_model("anthropic", &model.id)
                .unwrap_or_else(|| panic!("anthropic/{} missing", model.id));
            assert_eq!(
                get_supported_thinking_levels(direct),
                metadata_derived_levels(direct),
                "anthropic/{}: levels diverge from thinkingLevelMap",
                model.id
            );
        }
    }
}
