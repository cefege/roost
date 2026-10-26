//! Port of `packages/ai/src/env-api-keys.ts` @ pi 0.82.1 (2efa728).
//!
//! Known API-key environment variables per provider. The static mapping is a
//! data-level port of the full upstream table (T13 reuses it for the
//! remaining providers).
//!
//! Not in this table (provider-owned ambient logins, landed in T13):
//! - the Vertex ADC `<authenticated>` branch (`hasVertexAdcCredentials`,
//!   GOOGLE_CLOUD_PROJECT/LOCATION checks) — `providers/google_vertex.rs`
//! - the Amazon Bedrock ambient `<authenticated>` branch (AWS_PROFILE / IAM
//!   keys / bearer token / ECS / IRSA) — `providers/amazon_bedrock.rs`
//! - the Bun `/proc/self/environ` sandbox fallback (Bun-specific, see
//!   `utils/provider_env.rs`)

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;

pub const ANTHROPIC_FEDERATION_RULE_ID_ENV: &str = "ANTHROPIC_FEDERATION_RULE_ID";
pub const ANTHROPIC_ORGANIZATION_ID_ENV: &str = "ANTHROPIC_ORGANIZATION_ID";
pub const ANTHROPIC_SERVICE_ACCOUNT_ID_ENV: &str = "ANTHROPIC_SERVICE_ACCOUNT_ID";
pub const ANTHROPIC_IDENTITY_TOKEN_FILE_ENV: &str = "ANTHROPIC_IDENTITY_TOKEN_FILE";
pub const ANTHROPIC_WORKSPACE_ID_ENV: &str = "ANTHROPIC_WORKSPACE_ID";
pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// `getApiKeyEnvVars` — full static mapping, in upstream declaration order.
fn get_api_key_env_vars(provider: &str) -> Option<&'static [&'static str]> {
    if provider == "github-copilot" {
        return Some(&["COPILOT_GITHUB_TOKEN"]);
    }

    // ANTHROPIC_AUTH_TOKEN participates in env discovery/status, but
    // get_env_api_key() skips it because requests must pass it as
    // Authorization: Bearer.
    if provider == "anthropic" {
        return Some(&[
            ANTHROPIC_AUTH_TOKEN_ENV,
            ANTHROPIC_OAUTH_TOKEN_ENV,
            ANTHROPIC_API_KEY_ENV,
        ]);
    }

    match provider {
        "ant-ling" => Some(&["ANT_LING_API_KEY"]),
        "baseten" => Some(&["BASETEN_API_KEY"]),
        "qwen-token-plan" => Some(&["QWEN_TOKEN_PLAN_API_KEY"]),
        "qwen-token-plan-cn" => Some(&["QWEN_TOKEN_PLAN_CN_API_KEY"]),
        "qwen-token-plan-individual" => Some(&["QWEN_TOKEN_PLAN_API_KEY"]),
        "openai" => Some(&["OPENAI_API_KEY"]),
        "azure" => Some(&["AZURE_OPENAI_API_KEY"]),
        "nvidia" => Some(&["NVIDIA_API_KEY"]),
        "deepseek" => Some(&["DEEPSEEK_API_KEY"]),
        "google" => Some(&["GEMINI_API_KEY"]),
        "google-vertex" => Some(&["GOOGLE_CLOUD_API_KEY"]),
        "groq" => Some(&["GROQ_API_KEY"]),
        "cerebras" => Some(&["CEREBRAS_API_KEY"]),
        "xai" => Some(&["XAI_API_KEY"]),
        "radius" => Some(&["RADIUS_API_KEY"]),
        "openrouter" => Some(&["OPENROUTER_API_KEY"]),
        "vercel-ai-gateway" => Some(&["AI_GATEWAY_API_KEY"]),
        "zai" => Some(&["ZAI_API_KEY"]),
        "zai-coding-cn" => Some(&["ZAI_CODING_CN_API_KEY"]),
        "mistral" => Some(&["MISTRAL_API_KEY"]),
        "minimax" => Some(&["MINIMAX_API_KEY"]),
        "minimax-cn" => Some(&["MINIMAX_CN_API_KEY"]),
        "moonshotai" => Some(&["MOONSHOT_API_KEY"]),
        "moonshotai-cn" => Some(&["MOONSHOT_API_KEY"]),
        "huggingface" => Some(&["HF_TOKEN"]),
        "fireworks" => Some(&["FIREWORKS_API_KEY"]),
        "together" => Some(&["TOGETHER_API_KEY"]),
        "opencode" => Some(&["OPENCODE_API_KEY"]),
        "opencode-go" => Some(&["OPENCODE_API_KEY"]),
        "kimi-coding" => Some(&["KIMI_API_KEY"]),
        "meta" => Some(&["META_API_KEY"]),
        "cloudflare-workers-ai" => Some(&["CLOUDFLARE_API_KEY"]),
        "cloudflare-ai-gateway" => Some(&["CLOUDFLARE_API_KEY"]),
        "xiaomi" => Some(&["XIAOMI_API_KEY"]),
        "xiaomi-token-plan-cn" => Some(&["XIAOMI_TOKEN_PLAN_CN_API_KEY"]),
        "xiaomi-token-plan-ams" => Some(&["XIAOMI_TOKEN_PLAN_AMS_API_KEY"]),
        "xiaomi-token-plan-sgp" => Some(&["XIAOMI_TOKEN_PLAN_SGP_API_KEY"]),
        _ => None,
    }
}

/// `getApiKeyEnvVars` — the declared API-key environment variables for a
/// provider, regardless of whether they are present in the process
/// environment. The usage-provider framework (V16-05 FR-A) uses this to
/// tell a script which variable name carries its credential (the value
/// itself travels via the child process environment, never stdin).
pub fn api_key_env_vars(provider: &str) -> Option<&'static [&'static str]> {
    get_api_key_env_vars(provider)
}

/// `findEnvKeys` — configured environment variables that can provide an API
/// key for a provider. Only actual API key variables; ambient credential
/// sources (AWS profiles, ADC) are intentionally excluded.
pub fn find_env_keys(provider: &str, env: Option<&ProviderEnv>) -> Option<Vec<String>> {
    let env_vars = get_api_key_env_vars(provider)?;
    let found: Vec<String> = env_vars
        .iter()
        .filter(|env_var| get_provider_env_value(env_var, env).is_some())
        .map(|env_var| (*env_var).to_owned())
        .collect();
    if found.is_empty() { None } else { Some(found) }
}

/// `getEnvApiKey` — API key for a provider from known environment variables,
/// e.g. OPENAI_API_KEY. Never returns keys for providers that require OAuth
/// tokens.
pub fn get_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(env_keys) = find_env_keys(provider, env)
        && let Some(first) = env_keys.first()
    {
        // Anthropic: ANTHROPIC_AUTH_TOKEN must reach requests as an
        // Authorization: Bearer header, not as an api key — skip it.
        let api_key_env = if provider == "anthropic" {
            env_keys
                .iter()
                .find(|key| key.as_str() != ANTHROPIC_AUTH_TOKEN_ENV)
        } else {
            Some(first)
        };
        if let Some(api_key_env) = api_key_env {
            return get_provider_env_value(api_key_env, env);
        }
    }

    // google-vertex ADC `<authenticated>` (GOOGLE_CLOUD_PROJECT /
    // GCLOUD_PROJECT / GOOGLE_CLOUD_LOCATION) and amazon-bedrock ambient
    // `<authenticated>` (AWS_PROFILE / IAM keys / AWS_BEARER_TOKEN_BEDROCK)
    // are provider-owned ambient logins — they live in
    // `providers/google_vertex.rs` / `providers/amazon_bedrock.rs`, not in
    // this env-key table.

    None
}

#[cfg(test)]
mod tests {
    //! No dedicated upstream test file for `env-api-keys.ts`; coverage here
    //! pins the mapping-table data and the anthropic special case against
    //! the upstream source.

    use super::*;

    struct EnvGuard(Vec<&'static str>);

    impl EnvGuard {
        fn set(entries: &[(&'static str, &str)]) -> Self {
            // Distinct names per test (process env is global).
            for (name, value) in entries {
                rpi_test_env::set_var(name, value);
            }
            Self(entries.iter().map(|(name, _)| *name).collect())
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for name in &self.0 {
                rpi_test_env::remove_var(name);
            }
        }
    }

    #[test]
    fn static_mapping_table_matches_upstream() {
        // Spot-check the special cases plus entries across the whole table.
        assert_eq!(
            get_api_key_env_vars("github-copilot"),
            Some(&["COPILOT_GITHUB_TOKEN"][..])
        );
        assert_eq!(
            get_api_key_env_vars("anthropic"),
            Some(
                &[
                    ANTHROPIC_AUTH_TOKEN_ENV,
                    ANTHROPIC_OAUTH_TOKEN_ENV,
                    ANTHROPIC_API_KEY_ENV
                ][..]
            )
        );
        assert_eq!(
            get_api_key_env_vars("openai"),
            Some(&["OPENAI_API_KEY"][..])
        );
        assert_eq!(
            get_api_key_env_vars("google"),
            Some(&["GEMINI_API_KEY"][..])
        );
        // Shared env vars across provider ids.
        assert_eq!(
            get_api_key_env_vars("moonshotai-cn"),
            Some(&["MOONSHOT_API_KEY"][..])
        );
        assert_eq!(
            get_api_key_env_vars("opencode-go"),
            Some(&["OPENCODE_API_KEY"][..])
        );
        assert_eq!(
            get_api_key_env_vars("cloudflare-ai-gateway"),
            Some(&["CLOUDFLARE_API_KEY"][..])
        );
        // `meta: "META_API_KEY"` (env-api-keys.ts @ b73412a37, V15-15).
        assert_eq!(get_api_key_env_vars("meta"), Some(&["META_API_KEY"][..]));
        assert_eq!(get_api_key_env_vars("huggingface"), Some(&["HF_TOKEN"][..]));
        // Providers without a known env var (ambient-only).
        assert_eq!(get_api_key_env_vars("amazon-bedrock"), None);
        assert_eq!(get_api_key_env_vars("unknown-provider"), None);
    }

    #[test]
    fn find_env_keys_reports_only_set_variables() {
        let _guard = EnvGuard::set(&[("TEST_ENV_KEYS_OPENAI", "sk-test")]);
        // Unknown providers have no candidates.
        assert_eq!(find_env_keys("amazon-bedrock", None), None);
        // Known provider, variable unset.
        assert_eq!(find_env_keys("openai", None), None);
        // Scoped env override counts.
        let env = ProviderEnv::from([("OPENAI_API_KEY".to_owned(), "scoped".to_owned())]);
        assert_eq!(
            find_env_keys("openai", Some(&env)),
            Some(vec!["OPENAI_API_KEY".to_owned()])
        );
        // Empty-string values are falsy (upstream `!!getProviderEnvValue`).
        let env = ProviderEnv::from([("OPENAI_API_KEY".to_owned(), String::new())]);
        assert_eq!(find_env_keys("openai", Some(&env)), None);

        // Anthropic reports all three set variables, in table order.
        let env = ProviderEnv::from([
            (ANTHROPIC_AUTH_TOKEN_ENV.to_owned(), "auth-token".to_owned()),
            (ANTHROPIC_API_KEY_ENV.to_owned(), "api-key".to_owned()),
        ]);
        assert_eq!(
            find_env_keys("anthropic", Some(&env)),
            Some(vec![
                ANTHROPIC_AUTH_TOKEN_ENV.to_owned(),
                ANTHROPIC_API_KEY_ENV.to_owned()
            ])
        );
    }

    #[test]
    fn get_env_api_key_skips_anthropic_auth_token() {
        // AUTH_TOKEN alone: no api key (it is Bearer-header material).
        let env =
            ProviderEnv::from([(ANTHROPIC_AUTH_TOKEN_ENV.to_owned(), "auth-token".to_owned())]);
        assert_eq!(get_env_api_key("anthropic", Some(&env)), None);

        // OAUTH_TOKEN / API_KEY resolve as api keys, in priority order.
        let env = ProviderEnv::from([
            (ANTHROPIC_AUTH_TOKEN_ENV.to_owned(), "auth-token".to_owned()),
            (
                ANTHROPIC_OAUTH_TOKEN_ENV.to_owned(),
                "oauth-token".to_owned(),
            ),
            (ANTHROPIC_API_KEY_ENV.to_owned(), "api-key".to_owned()),
        ]);
        assert_eq!(
            get_env_api_key("anthropic", Some(&env)).as_deref(),
            Some("oauth-token")
        );

        let env = ProviderEnv::from([(ANTHROPIC_API_KEY_ENV.to_owned(), "api-key".to_owned())]);
        assert_eq!(
            get_env_api_key("anthropic", Some(&env)).as_deref(),
            Some("api-key")
        );
    }

    #[test]
    fn get_env_api_key_resolves_from_table_and_scoped_env() {
        let env = ProviderEnv::from([("OPENAI_API_KEY".to_owned(), "scoped-key".to_owned())]);
        assert_eq!(
            get_env_api_key("openai", Some(&env)).as_deref(),
            Some("scoped-key")
        );
        assert_eq!(get_env_api_key("openai", None), None);
        assert_eq!(get_env_api_key("unknown-provider", None), None);

        // T13 scope: vertex/bedrock ambient do not return `<authenticated>`.
        let env = ProviderEnv::from([
            ("GOOGLE_CLOUD_PROJECT".to_owned(), "p".to_owned()),
            ("GOOGLE_CLOUD_LOCATION".to_owned(), "l".to_owned()),
        ]);
        assert_eq!(get_env_api_key("google-vertex", Some(&env)), None);
        let env = ProviderEnv::from([("AWS_PROFILE".to_owned(), "default".to_owned())]);
        assert_eq!(get_env_api_key("amazon-bedrock", Some(&env)), None);
    }
}
