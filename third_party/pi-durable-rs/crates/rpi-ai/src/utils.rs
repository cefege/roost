//! Cross-cutting utilities, mirroring `packages/ai/src/utils/` (design §3.6).
//!
//! `transform_messages` lives here per design §3.6 (upstream keeps it under
//! `api/`); `cost` holds `calculateCost` (upstream `models.ts`).

pub mod cost;
pub mod custom_fetch;
/// (removed #9548): deferred tool loading via `deferred_tools` was replaced
/// by transcript system-message tool declarations; see `utils/transcript.rs`.
pub mod error_body;
pub mod estimate;
pub mod event_stream;
pub mod hash;
pub mod headers;
pub mod http_proxy;
pub mod json_parse;
pub mod overflow;
pub mod provider_env;
pub mod provider_retry;
pub mod retry;
pub mod rpi_user_agent;
pub mod sanitize_unicode;
pub mod text;
pub mod transcript;
pub mod transform_messages;
pub mod transport_error;
pub mod uuid;
pub mod validation;
