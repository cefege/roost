//! Port of `packages/ai/src/auth/oauth/` @ pi abe508e1b (v1.1.0) — OAuth flow
//! building blocks: PKCE (`pkce`), the RFC 8628 device-code polling framework
//! (`device_code`), the shared loopback callback server (`callback_server`),
//! the shared browser page (`callback_page`), the flow registry (`load`) and
//! the provider flows (`anthropic`, `github_copilot`, `kimi_coding`,
//! `openai_chatgpt`, `openai_codex`, `openrouter`, `radius`, `xai`).

pub mod anthropic;
pub mod callback_page;
pub mod callback_server;
pub mod device_code;
pub mod github_copilot;
pub mod kimi_coding;
pub mod load;
pub mod meta;
pub mod openai_chatgpt;
pub mod openai_codex;
pub mod openrouter;
pub mod pkce;
pub mod radius;
pub mod xai;

pub use anthropic::anthropic_oauth;
pub use callback_page::{oauth_error_html, oauth_success_html};
pub use callback_server::{
    CallbackOrManual, CompleteFn, ManualPrompt, OAuthCallbackServer, OAuthCallbackServerOptions,
    default_callback_host, wait_for_callback_or_manual_input,
};
pub use device_code::{DeviceCodePollOptions, DeviceCodePollResult, poll_oauth_device_code_flow};
pub use github_copilot::github_copilot_oauth;
pub use kimi_coding::kimi_coding_oauth;
pub use load::{OAuthFlowLoader, load_meta_oauth, load_oauth_flow, load_xai_oauth};
pub use meta::meta_oauth;
pub use openai_chatgpt::openai_chatgpt_oauth;
pub use openai_codex::openai_codex_oauth;
pub use openrouter::openrouter_oauth;
pub use pkce::{Pkce, generate_pkce};
pub use radius::{RadiusOAuthOptions, create_radius_oauth};
pub use xai::xai_oauth;
