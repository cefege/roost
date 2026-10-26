//! Built-in model catalog validation (T13 W4).
//!
//! Three layers:
//! 1. Integrity: every vendored `src/providers/data/*.json` matches the sha256
//!    recorded in the vendored `.manifest.json` (upstream `model-data.ts`).
//!    At refresh time the refresh script additionally byte-compares the
//!    vendored set against the throwaway upstream generation (V14-09 L2);
//!    the external/pi worktree itself stays unmodified (its data snapshot is
//!    git-ignored and goes stale after every pin bump, so it cannot be a
//!    test dependency).
//! 2. Provenance: the vendored file set matches the manifest's file map.
//! 3. Runtime shape: the parsed catalog (`rpi_ai::generated`) preserves
//!    provider set, model order, and every field of the vendored JSON.

use std::collections::BTreeMap;
use std::path::PathBuf;

use rpi_ai::auth::{find_env_keys, get_env_api_key};
use rpi_ai::generated::{builtin_catalog, get_builtin_model_data_generated_at, get_builtin_models};
use rpi_ai::types::{InputModality, MaxTokensField, ModelCompat};
use rpi_ai::types::{ProviderEnv, ThinkingFormat};
use sha2::Digest;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/providers/data")
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn provider_files(dir: &std::path::Path) -> BTreeMap<String, PathBuf> {
    std::fs::read_dir(dir)
        .expect("read data dir")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .into_string()
                .expect("utf-8 name")
        })
        .filter(|name| name.ends_with(".json") && !name.starts_with('.'))
        .map(|name| (name.clone(), dir.join(name)))
        .collect()
}

#[test]
fn test_vendored_files_match_manifest_sha256() {
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data_dir().join(".manifest.json")).expect("manifest"),
    )
    .expect("manifest json");
    let files = manifest["files"].as_object().expect("files");
    assert_eq!(files.len(), 42);
    for (name, hash) in files {
        let bytes = std::fs::read(data_dir().join(name)).expect("vendored file");
        assert_eq!(
            &sha256_hex(&bytes),
            hash.as_str().expect("hash string"),
            "sha256 mismatch for {name}"
        );
    }
}

/// Provenance: the vendored file set equals the manifest's file map (the
/// manifest travels with the vendored generation; its sha256s are pinned by
/// `test_vendored_files_match_manifest_sha256`).
#[test]
fn test_vendored_set_matches_manifest_file_map() {
    let vendored = provider_files(&data_dir());
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(data_dir().join(".manifest.json")).expect("manifest"),
    )
    .expect("manifest json");
    let files = manifest["files"].as_object().expect("files");
    let mut manifest_names: Vec<&str> = files.keys().map(|k| k.as_str()).collect();
    manifest_names.sort();
    let vendored_names: Vec<String> = vendored.keys().cloned().collect();
    assert_eq!(
        vendored_names, manifest_names,
        "vendored file set diverges from manifest"
    );
}

/// TS has a single `number` type; our typed cost fields are `f64` and
/// re-serialize integer JSON as `0.0`. Normalize every number to f64 so the
/// comparison is about field content, not int/float representation.
fn normalize_numbers(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Number(n) => serde_json::Value::from(n.as_f64().expect("finite")),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(normalize_numbers).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), normalize_numbers(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Gateway-passthrough keys the Radius public catalog carries on every
/// entry (`enabled` / `lab` / `providers` routing metadata from
/// `{gateway}/v1/config`). Upstream TS spreads them into the Model objects
/// untyped (nothing consumes them); our typed [`Model`] drops them at parse
/// time — same intentional difference as the dynamic-path sanitize
/// (D-032, `radius_config.rs` header). Stripped here so the field-by-field
/// comparison stays exact for every declared field.
const RADIUS_PASSTHROUGH_KEYS: [&str; 3] = ["enabled", "lab", "providers"];

/// Runtime shape: every model in every vendored provider JSON survives the
/// typed parse (`builtin_catalog`) and re-serialization field-for-field.
/// Since V14-09 the comparison source is the vendored JSON itself (the
/// external/pi mirror is git-ignored, stale after pin bumps, and absent on
/// fresh checkouts); byte-identity with the throwaway upstream generation is
/// established at refresh time by `refresh-model-catalog.sh` (sha256).
#[test]
fn test_catalog_field_by_field_roundtrip() {
    let catalog = builtin_catalog().expect("catalog parses");
    let vendored = provider_files(&data_dir());
    let mut total = 0usize;

    for (name, path) in &vendored {
        let provider = name.trim_end_matches(".json");
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("vendored file"))
                .expect("vendored json");
        let groups = json.as_object().expect("groups");

        // Upstream flattenModelCatalog order: groups in file order, models in
        // group order (both key-sorted by the generator, schema v6
        // `type:id` composite keys).
        let expected: Vec<(&str, &serde_json::Value)> = groups
            .values()
            .flat_map(|group| {
                group
                    .as_object()
                    .expect("model group")
                    .values()
                    .map(|model| {
                        (
                            model
                                .get("id")
                                .and_then(|id| id.as_str())
                                .expect("model id"),
                            model,
                        )
                    })
            })
            .collect();

        let models = catalog.all_models(provider);
        assert_eq!(
            models.len(),
            expected.len(),
            "model count diverges for {provider}"
        );
        for (model, (expected_id, expected_value)) in models.iter().zip(&expected) {
            assert_eq!(
                model.id(),
                *expected_id,
                "model order diverges for {provider}"
            );
            assert_eq!(model.provider(), provider);
            let mut expected_value = (*expected_value).clone();
            if provider == "radius"
                && let serde_json::Value::Object(map) = &mut expected_value
            {
                for key in RADIUS_PASSTHROUGH_KEYS {
                    map.remove(key);
                }
                // `pi-messages` reads back as `rpi-messages` (the de-pi
                // brand rename, `normalize_api_kind`; radius is the only
                // catalog provider on that API).
                if map.get("api") == Some(&serde_json::Value::String("pi-messages".to_owned())) {
                    map.insert(
                        "api".to_owned(),
                        serde_json::Value::String("rpi-messages".to_owned()),
                    );
                }
            }
            // Schema v6 emits an explicit `"type":"chat"` on every chat
            // entry; rpi's typed chat [`Model`] has no type field (chat is
            // the default and upstream `type?: "chat"` is optional) and
            // drops the tag on parse. Semantically identical — normalized
            // here; image/classifier keep their `type`.
            if let serde_json::Value::Object(map) = &mut expected_value
                && map.get("type") == Some(&serde_json::Value::String("chat".to_owned()))
            {
                map.remove("type");
            }
            let actual = normalize_numbers(&serde_json::to_value(model).expect("serialize model"));
            assert_eq!(
                &actual,
                &normalize_numbers(&expected_value),
                "field-level divergence for {provider}:{expected_id}"
            );
            total += 1;
        }
    }
    assert_eq!(total, 1731);
}

/// FR-E R2 (V14-09): every `compat` key present in the vendored JSON must
/// survive a `ModelCompat` parse → re-serialize roundtrip. `ModelCompat` has
/// no `deny_unknown_fields`, so an upstream compat key that was not landed
/// in the struct would otherwise be silently dropped at parse time instead
/// of failing loudly.
#[test]
fn test_compat_keys_roundtrip_without_loss() {
    let mut total = 0usize;
    for (name, path) in provider_files(&data_dir()) {
        let json: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).expect("vendored file"))
                .expect("vendored json");
        let groups = json.as_object().expect("groups");
        for group in groups.values() {
            for (id, model) in group.as_object().expect("model group") {
                let Some(raw_compat) = model.get("compat") else {
                    continue;
                };
                let compat: ModelCompat =
                    serde_json::from_value(raw_compat.clone()).expect("parse compat");
                let roundtripped = serde_json::to_value(&compat).expect("serialize compat");
                let out = roundtripped.as_object().expect("compat object");
                for key in raw_compat.as_object().expect("compat object").keys() {
                    assert!(
                        out.contains_key(key),
                        "compat key `{key}` lost in roundtrip for {name}:{id} — un-landed upstream compat field?"
                    );
                }
                total += 1;
            }
        }
    }
    assert!(total > 0, "no compat objects found in vendored catalog");
}

#[test]
fn test_catalog_accessors_and_generated_at() {
    // Every registry provider with a catalog entry yields models; classifier-
    // only providers (typesafe) have no chat models but carry schema-v6
    // entries (V16-07 wired their spec + system-one adapter).
    for spec in rpi_ai::providers::BUILTIN_PROVIDERS {
        if spec.in_catalog {
            assert!(
                !get_builtin_models(spec.id).is_empty()
                    || !rpi_ai::generated::get_builtin_all_models(spec.id).is_empty(),
                "{}",
                spec.id
            );
        }
    }
    // Pinned to the vendored .manifest.json generatedAt
    // (2026-10-19T09:55:38.636Z); update on catalog refresh.
    assert_eq!(
        get_builtin_model_data_generated_at(),
        Some(1_792_403_738_636)
    );
}

// ---------------------------------------------------------------------------
// T26: Baseten golden tests (port of baseten-models.test.ts @ c1019d920)
// ---------------------------------------------------------------------------

fn get_model(provider: &str, id: &str) -> rpi_ai::types::Model {
    get_builtin_models(provider)
        .iter()
        .find(|m| m.id == id)
        .unwrap_or_else(|| panic!("model {provider}/{id} not found"))
        .clone()
}

/// "registers GLM 5.2 as the default OpenAI-compatible reasoning model"
/// (baseten-models.test.ts).
#[test]
fn test_baseten_glm52_reasoning_model_fields() {
    let model = get_model("baseten", "zai-org/GLM-5.2");
    assert_eq!(model.api.as_str(), "openai-completions");
    assert_eq!(model.provider, "baseten");
    assert_eq!(model.base_url, "https://inference.baseten.co/v1");
    assert!(model.reasoning);
    // thinkingLevelMap: off→none, high→high, max→max (rest null)
    let tlm = model.thinking_level_map.as_ref().expect("thinkingLevelMap");
    assert_eq!(
        tlm.get(&rpi_ai::types::ModelThinkingLevel::Off),
        Some(&Some("none".to_owned()))
    );
    assert_eq!(
        tlm.get(&rpi_ai::types::ModelThinkingLevel::High),
        Some(&Some("high".to_owned()))
    );
    assert_eq!(
        tlm.get(&rpi_ai::types::ModelThinkingLevel::Max),
        Some(&Some("max".to_owned()))
    );
    assert_eq!(model.input, vec![InputModality::Text]);
    assert_eq!(model.context_window, 1048576);
    assert_eq!(model.max_tokens, 262144);
    assert_eq!(model.cost.rates.input, 1.4);
    assert_eq!(model.cost.rates.output, 4.4);
    assert_eq!(model.cost.rates.cache_read, 0.3);
    assert_eq!(model.cost.rates.cache_write, 0.0);

    let compat = model.compat.as_ref().expect("compat");
    assert_eq!(compat.supports_store, Some(false));
    assert_eq!(compat.supports_developer_role, Some(false));
    assert_eq!(compat.supports_reasoning_effort, Some(true));
    assert_eq!(compat.supports_usage_in_streaming, Some(true));
    assert_eq!(compat.max_tokens_field, Some(MaxTokensField::MaxTokens));
    assert_eq!(compat.supports_strict_mode, Some(true));
    assert_eq!(compat.supports_long_cache_retention, Some(false));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Baseten));
    // chatTemplateArgs: { enable_thinking: { $var: "thinking.enabled" } }
    let cta = compat
        .chat_template_args
        .as_ref()
        .expect("chatTemplateArgs");
    assert!(cta.contains_key("enable_thinking"));
}

/// "models Kimi K2.6 reasoning as an explicit off/on toggle"
/// (baseten-models.test.ts).
#[test]
fn test_baseten_kimi_k26_toggle_thinking() {
    let model = get_model("baseten", "moonshotai/Kimi-K2.6");
    let tlm = model.thinking_level_map.as_ref().expect("thinkingLevelMap");
    // off→off, high→high (rest null): explicit off/on toggle
    assert_eq!(
        tlm.get(&rpi_ai::types::ModelThinkingLevel::Off),
        Some(&Some("off".to_owned()))
    );
    assert_eq!(
        tlm.get(&rpi_ai::types::ModelThinkingLevel::High),
        Some(&Some("high".to_owned()))
    );
    assert_eq!(
        tlm.get(&rpi_ai::types::ModelThinkingLevel::Max),
        Some(&None)
    );

    let compat = model.compat.as_ref().expect("compat");
    assert_eq!(compat.supports_reasoning_effort, Some(false));
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Baseten));
    assert!(compat.chat_template_args.is_some());
}

/// "resolves BASETEN_API_KEY from the environment" (baseten-models.test.ts).
#[test]
fn test_baseten_env_key_resolution() {
    let env = ProviderEnv::from([("BASETEN_API_KEY".to_owned(), "test-baseten-key".to_owned())]);
    assert_eq!(
        find_env_keys("baseten", Some(&env)),
        Some(vec!["BASETEN_API_KEY".to_owned()])
    );
    assert_eq!(
        get_env_api_key("baseten", Some(&env)).as_deref(),
        Some("test-baseten-key")
    );
}

/// All Baseten models are non-deprecated (the generator's
/// `processBasetenModels` skips status=="deprecated").
#[test]
fn test_baseten_deprecated_models_filtered() {
    let models = get_builtin_models("baseten");
    // 24 models = upstream catalog after deprecated filtering (2026-10-19
    // snapshot; models.dev added the DeepSeek V4.1 Flash 1017 pair).
    assert_eq!(
        models.len(),
        24,
        "Baseten should have 24 non-deprecated models"
    );
    // No model name or id contains "deprecated".
    for model in models.iter() {
        assert!(
            !model.id.to_lowercase().contains("deprecat")
                && !model.name.to_lowercase().contains("deprecat"),
            "deprecated model leaked: {}",
            model.id
        );
    }
}

// ---------------------------------------------------------------------------
// T26: Qwen Token Plan Individual strict whitelist
// (port of generate-models-strict.test.ts @ c03d78bdc)
// ---------------------------------------------------------------------------

/// The upstream `QWEN_TOKEN_PLAN_INDIVIDUAL_MODEL_IDS` whitelist has exactly 9
/// IDs; the vendored catalog must match. Upstream drift = explicit failure.
/// Port of `assertExactModelIds` intent (generate-models-strict.test.ts).
/// V14-09: deepseek-v4-pro-0813 added by 6db110e6f, qwen3.8-flash by
/// 265a33393 (whitelist at generate-models.ts:309-319 @ 9841914).
#[test]
fn test_qwen_token_plan_individual_whitelist_exact() {
    const EXPECTED: &[&str] = &[
        "deepseek-v4-flash-0731",
        "deepseek-v4-pro",
        "deepseek-v4-pro-0813",
        "glm-5.2",
        "qwen3.6-flash",
        "qwen3.7-max",
        "qwen3.7-plus",
        "qwen3.8-flash",
        "qwen3.8-max",
    ];
    let models = get_builtin_models("qwen-token-plan-individual");
    let mut actual: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    actual.sort();
    let mut expected: Vec<&str> = EXPECTED.to_vec();
    expected.sort();
    assert_eq!(
        actual, expected,
        "qwen-token-plan-individual whitelist diverged"
    );
    assert_eq!(models.len(), 9);
}

/// Individual variant shares QWEN_TOKEN_PLAN_API_KEY and the international
/// endpoint (port of qwen-token-plan-individual.ts).
#[test]
fn test_qwen_token_plan_individual_shares_api_key() {
    let env = ProviderEnv::from([(
        "QWEN_TOKEN_PLAN_API_KEY".to_owned(),
        "shared-key".to_owned(),
    )]);
    assert_eq!(
        find_env_keys("qwen-token-plan-individual", Some(&env)),
        Some(vec!["QWEN_TOKEN_PLAN_API_KEY".to_owned()])
    );
    let models = get_builtin_models("qwen-token-plan-individual");
    let first = models.first().expect("has models");
    assert_eq!(
        first.base_url,
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
    );
}

// ---------------------------------------------------------------------------
// T26: Catalog correction assertions (8 items, data baked into vendored JSON)
// ---------------------------------------------------------------------------

/// 1. qwen3.8-max-preview → qwen3.8-max (2f7f75a20, #7670).
#[test]
fn test_correction_qwen38_max_rename() {
    let models = get_builtin_models("qwen-token-plan-individual");
    let ids: Vec<&str> = models.iter().map(|m| m.id.as_str()).collect();
    assert!(
        ids.contains(&"qwen3.8-max"),
        "qwen3.8-max should be present"
    );
    assert!(
        !ids.contains(&"qwen3.8-max-preview"),
        "qwen3.8-max-preview should be absent"
    );
}

/// 2. Copilot Grok 4.5 routed via Responses API (720f0e8ee, #7560).
#[test]
fn test_correction_copilot_grok45_responses() {
    let model = get_model("github-copilot", "grok-4.5");
    assert_eq!(model.api.as_str(), "openai-responses");
}

/// 2c. #9423 (12f59336a, FR-D R2): the retired deepseek-v4-flash / vision
/// aliases are gone, the canonical `deepseek-flash` (V4.1 Flash) is
/// published with refreshed pricing. (Catalog data was refreshed with the
/// 2026-09-14 D-101 snapshot which already includes this fix; the assertion
/// pins the shipped state.)
#[test]
fn test_correction_deepseek_flash_catalog() {
    let ids: Vec<&str> = get_builtin_models("deepseek")
        .iter()
        .map(|m| m.id.as_str())
        .collect();
    assert!(ids.contains(&"deepseek-flash"), "ids: {ids:?}");
    assert!(
        !ids.contains(&"deepseek-v4-flash"),
        "retired alias present: {ids:?}"
    );
    assert!(
        !ids.contains(&"deepseek-v4-flash-vision-exp"),
        "retired alias present: {ids:?}"
    );
    let model = get_model("deepseek", "deepseek-flash");
    let cost = &model.cost.rates;
    assert_eq!(cost.input, 0.3);
    assert_eq!(cost.output, 1.2);
}

/// 2b. All Copilot GPT models route through the Responses API (#9253,
/// 7d8ab31a4 — upstream `model-catalog-types.test.ts` "routes all GitHub
/// Copilot GPT models through the Responses API"; generator rule
/// `needsResponsesApi = gpt- | grok- | oswe | mai-`, incl. GPT-6 Astra).
#[test]
fn test_correction_copilot_gpt_models_responses() {
    let gpt_models: Vec<rpi_ai::types::Model> =
        rpi_ai::generated::get_builtin_models("github-copilot")
            .iter()
            .filter(|model| model.id.starts_with("gpt-"))
            .cloned()
            .collect();
    assert!(
        !gpt_models.is_empty(),
        "catalog must contain Copilot GPT models"
    );
    for model in &gpt_models {
        assert_eq!(
            model.api.as_str(),
            "openai-responses",
            "Copilot model {} must route via Responses",
            model.id
        );
    }
    // GPT-6 Astra is the #9253 regression model (upstream #9209).
    assert_eq!(
        get_model("github-copilot", "gpt-6-astra").api.as_str(),
        "openai-responses"
    );
}

/// 3. Fireworks Kimi K3: openai-completions + reasoning-effort + deferred
///    tools (a688e257c, #7199).
#[test]
fn test_correction_fireworks_kimi_k3_compat() {
    let model = get_model("fireworks", "accounts/fireworks/models/kimi-k3");
    assert_eq!(model.api.as_str(), "openai-completions");
    let compat = model.compat.as_ref().expect("compat");
    assert_eq!(compat.thinking_format, Some(ThinkingFormat::Openai));
    assert_eq!(
        compat.requires_reasoning_content_on_assistant_messages,
        Some(true)
    );
    // Post-#9548 catalog shape: the Kimi deferred-tools face is expressed as
    // mid-conversation system/tool-addition support (9e05370b2 rework;
    // deferredToolsMode left the catalog). Wire-side convergence: V15-06
    // (T-V15-02-1).
    assert_eq!(compat.supports_mid_convo_system_messages, Some(true));
    assert_eq!(compat.supports_mid_convo_tool_additions, Some(true));
    assert_eq!(compat.supports_mid_convo_tool_changes, None);
    assert_eq!(compat.send_session_affinity_headers, Some(true));
}

/// 4. Fireworks GLM 5.3: session affinity + no long cache retention
///    (b9497c8c1, #7676; id renamed glm-5p2 → glm-5p3 in the 2026-10-01
///    snapshot).
#[test]
fn test_correction_fireworks_glm53_session_affinity() {
    let model = get_model("fireworks", "accounts/fireworks/models/glm-5p3");
    assert_eq!(model.api.as_str(), "openai-completions");
    let compat = model.compat.as_ref().expect("compat");
    assert_eq!(compat.send_session_affinity_headers, Some(true));
    assert_eq!(compat.supports_long_cache_retention, Some(false));
}

/// 5. GPT-5.6 Terra/Luna price reduction (b889a0ce3).
#[test]
fn test_correction_gpt56_pricing() {
    let luna = get_model("openai", "gpt-5.6-luna");
    assert_eq!(luna.cost.rates.input, 0.2);
    assert_eq!(luna.cost.rates.output, 1.2);
    assert_eq!(luna.cost.rates.cache_read, 0.02);
    assert_eq!(luna.cost.rates.cache_write, 0.25);

    let terra = get_model("openai", "gpt-5.6-terra");
    assert_eq!(terra.cost.rates.input, 2.0);
    assert_eq!(terra.cost.rates.output, 12.0);
    assert_eq!(terra.cost.rates.cache_read, 0.2);
    assert_eq!(terra.cost.rates.cache_write, 2.5);
}

/// 6. Groq Qwen reasoning override → qwen/qwen3.6-27b (71f6c25c3).
#[test]
fn test_correction_groq_qwen_reasoning_override() {
    let model = get_model("groq", "qwen/qwen3.6-27b");
    let tlm = model.thinking_level_map.as_ref().expect("thinkingLevelMap");
    assert_eq!(
        tlm.get(&rpi_ai::types::ModelThinkingLevel::High),
        Some(&Some("default".to_owned()))
    );
    // The old qwen/qwen3-32b should be absent.
    let models = get_builtin_models("groq");
    assert!(
        !models.iter().any(|m| m.id == "qwen/qwen3-32b"),
        "old qwen/qwen3-32b should be replaced by qwen/qwen3.6-27b"
    );
}

/// 7. OpenCode Go display name (05558a792, #7157).
#[test]
fn test_correction_opencode_go_display_name() {
    let go_provider = rpi_ai::providers::builtin_providers()
        .into_iter()
        .find(|p| p.id() == "opencode-go")
        .expect("opencode-go provider");
    assert_eq!(go_provider.name(), "OpenCode Go");
}

/// 8. Copilot policy fallback is tested in auth/oauth/github_copilot.rs
///    (parse_available_model_ids_policy_fallback_* tests). This test
///    verifies the Individual-endpoint base URL is correct.
#[test]
fn test_correction_copilot_individual_endpoint() {
    let copilot = rpi_ai::providers::builtin_providers()
        .into_iter()
        .find(|p| p.id() == "github-copilot")
        .expect("github-copilot provider");
    assert_eq!(
        copilot.base_url(),
        Some("https://api.individual.githubcopilot.com")
    );
}

/// #10326 (`4665fafb4`): Bedrock OpenAI models keep their models.dev pricing
/// tiers, so long-context requests cost the long-context rate.
#[test]
fn test_correction_bedrock_openai_cost_tiers() {
    for id in ["global.openai.gpt-6-sol", "us.openai.gpt-5.6-luna"] {
        let model = get_model("amazon-bedrock", id);
        let tiers = model
            .cost
            .tiers
            .as_ref()
            .unwrap_or_else(|| panic!("{id} must carry pricing tiers"));
        assert!(
            tiers.iter().any(|tier| tier.input_tokens_above == 272_000),
            "{id}: {tiers:?}"
        );
        assert!(
            tiers
                .iter()
                .any(|tier| tier.rates.input > model.cost.rates.input),
            "{id}: the long-context tier must be more expensive"
        );
    }
}

/// `c10bfb0d7`: the Cloudflare AI Gateway `/anthropic` passthrough forwards
/// the model id unchanged, so the catalog uses dashed Claude ids.
#[test]
fn test_correction_cloudflare_gateway_dashed_claude_ids() {
    for id in ["claude-opus-5-5", "claude-haiku-4-5", "claude-sonnet-5-5"] {
        let model = get_model("cloudflare-ai-gateway", id);
        assert_eq!(model.id, id);
        assert_eq!(model.api.as_str(), "anthropic-messages");
    }
    // No dotted ids survive (Anthropic rejects them with a 404).
    assert!(
        get_builtin_models("cloudflare-ai-gateway")
            .iter()
            .filter(|model| model.api.as_str() == "anthropic-messages")
            .all(|model| !model.id.contains('.')),
        "dotted Claude ids must not ship"
    );
}
