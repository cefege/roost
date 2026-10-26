//! Port of `packages/ai/src/auth/oauth/load.ts` @ pi 0.82.1 (2efa728).
//!
//! Upstream, each `loadXxxOAuth` dynamic-imports its flow module so bundlers
//! keep Node-only flow code (`node:http` callback servers, `node:crypto`
//! PKCE) out of browser bundles, and `registerBundledOAuthFlowLoaders`
//! swaps in statically-bundled loaders for standalone Bun binaries. rpi is a
//! native binary — every flow is already statically linked, so the whole
//! trick collapses to the "bundled loaders" form: a registry of zero-arg
//! flow constructors keyed by provider id, plus the named loaders (upstream
//! function names preserved). `radius` is the upstream exception — it takes
//! `{ name, gateway }` options and uses [`super::create_radius_oauth`]
//! instead of an entry here.

use std::sync::Arc;

use super::super::types::OAuthAuth;
use super::{
    anthropic::anthropic_oauth, github_copilot::github_copilot_oauth,
    kimi_coding::kimi_coding_oauth, meta::meta_oauth, openai_chatgpt::openai_chatgpt_oauth,
    openai_codex::openai_codex_oauth, openrouter::openrouter_oauth, xai::xai_oauth,
};

/// `OAuthFlowLoaders` — a statically-bundled flow loader (upstream
/// `OAuthFlowLoaders[id]`, minus the option-taking `radius`).
pub type OAuthFlowLoader = fn() -> Arc<dyn OAuthAuth>;

/// `loadAnthropicOAuth`.
pub fn load_anthropic_oauth() -> Arc<dyn OAuthAuth> {
    anthropic_oauth()
}

/// `loadOpenAICodexOAuth`.
pub fn load_openai_codex_oauth() -> Arc<dyn OAuthAuth> {
    openai_codex_oauth()
}

/// `loadOpenAIChatGPTOAuth` (`02eed88fd`).
pub fn load_openai_chatgpt_oauth() -> Arc<dyn OAuthAuth> {
    openai_chatgpt_oauth()
}

/// `loadGitHubCopilotOAuth`.
pub fn load_github_copilot_oauth() -> Arc<dyn OAuthAuth> {
    github_copilot_oauth()
}

/// `loadOpenRouterOAuth`.
pub fn load_openrouter_oauth() -> Arc<dyn OAuthAuth> {
    openrouter_oauth()
}

/// `loadKimiCodingOAuth`.
pub fn load_kimi_coding_oauth() -> Arc<dyn OAuthAuth> {
    kimi_coding_oauth()
}

/// `loadMetaOAuth` (b73412a37, #9096).
pub fn load_meta_oauth() -> Arc<dyn OAuthAuth> {
    meta_oauth()
}

/// `loadXaiOAuth`.
pub fn load_xai_oauth() -> Arc<dyn OAuthAuth> {
    xai_oauth()
}

/// The registry: provider id → zero-arg flow constructor (the upstream
/// `registerBundledOAuthFlowLoaders` table).
pub fn oauth_flow_loaders() -> &'static [(&'static str, OAuthFlowLoader)] {
    &[
        ("anthropic", load_anthropic_oauth),
        ("openai-codex", load_openai_codex_oauth),
        ("openai", load_openai_chatgpt_oauth),
        ("github-copilot", load_github_copilot_oauth),
        ("openrouter", load_openrouter_oauth),
        ("kimi-coding", load_kimi_coding_oauth),
        ("meta", load_meta_oauth),
        ("xai", load_xai_oauth),
    ]
}

/// Registry lookup — the bundled-loaders branch of every upstream
/// `loadXxxOAuth` (`if (bundledLoaders) return bundledLoaders.xxx()`).
/// `None` for unknown ids (and for `radius`, which needs options).
pub fn load_oauth_flow(provider_id: &str) -> Option<Arc<dyn OAuthAuth>> {
    oauth_flow_loaders()
        .iter()
        .find(|(id, _)| *id == provider_id)
        .map(|(_, loader)| loader())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every registered id loads a flow under its upstream display name;
    /// unknown ids (and the option-taking radius) resolve to `None`.
    #[test]
    fn registry_lookup_covers_the_landed_flows() {
        let expected = [
            ("anthropic", "Anthropic (Claude Pro/Max)"),
            ("openai-codex", "OpenAI (ChatGPT Plus/Pro)"),
            ("openai", "OpenAI (ChatGPT subscription)"),
            ("github-copilot", "GitHub Copilot"),
            ("openrouter", "OpenRouter OAuth"),
            ("kimi-coding", "Kimi Code (subscription)"),
            ("meta", "Meta (Muse subscription)"),
            ("xai", "xAI (Grok/X subscription)"),
        ];
        assert_eq!(oauth_flow_loaders().len(), expected.len());
        for (id, name) in expected {
            let flow = load_oauth_flow(id).expect("loader");
            assert_eq!(flow.name(), name);
        }
        assert!(load_oauth_flow("radius").is_none());
        assert!(load_oauth_flow("does-not-exist").is_none());
    }
}
