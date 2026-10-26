//! Port of `packages/ai/src/auth/oauth/github-copilot.ts` @ pi `9841914`
//! (v0.85.0+) — GitHub Copilot OAuth: RFC 8628 device code flow against
//! github.com or a GitHub Enterprise domain, GitHub-token → Copilot-token
//! exchange, post-login policy-enable for the account's unconfigured known
//! models (`b3edf0170` / `d5278eaac` / `086c32e74` / `55b0db4d3`, #6187 /#7850:
//! known ∩ tool-capable ∩ `policy.state == "unconfigured"`, enabled
//! sequentially with 429 budget-retry honoring `retry-after`), and the
//! per-account `baseUrl` / `availableModelIds` derivation with
//! Individual-endpoint policy fallback.
//!
//! Test seams (upstream stubs the global `fetch`; here the seam is a
//! constructor field, minimal-intrusion — same precedent as
//! `anthropic.rs`'s `token_url`/`callback_port`):
//! - `authority`: when set, every `https://{host}/{path}` URL the flow builds
//!   is rewritten to `http://{authority}/{host}/{path}` before dispatch, so a
//!   single loopback mock can stand in for github.com, `api.*` and enterprise
//!   hosts while the recorded path still shows which host the flow targeted.
//!   `to_auth` (side-effect-free) never rewrites.
//!
//! Intentional differences:
//! - `Date.now()` math uses `SystemTime` milliseconds; `expires_at` /
//!   `expires_in` parse as `f64` (JS `number`) and narrow into the
//!   millisecond/`u64` fields of [`OAuthCredential`]/[`AuthEvent`];
//! - `fetchJson` error text uses reqwest's status canonical reason for
//!   `statusText` and reqwest's error text for network failures (same
//!   precedent as D-009's `formatErrorDetails` approximation);
//! - `fetchWithRateLimitRetry`'s per-request `AbortSignal.timeout(5000)` and
//!   budget `AbortSignal.timeout(maxElapsedMs)` become reqwest per-request
//!   timeouts plus an explicit deadline check before each retry sleep
//!   (upstream returns the response when `delayMs >= retryDeadline - now`,
//!   so the budget never needs to abort an in-flight sleep);
//! - `retry-after` seconds parse with Rust's strict `f64` parse instead of
//!   JS `parseFloat` prefix semantics (a "120abc"-style value would parse in
//!   JS; servers send clean numbers);
//! - the known-models list for policy-enable is the vendored catalog
//!   (`get_builtin_models("github-copilot")` — the same
//!   `github-copilot.json` upstream flattens into `GITHUB_COPILOT_MODELS`).

use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;

use super::super::interaction::{AuthEvent, AuthInteraction, AuthPrompt};
use super::super::resolve::{ModelsError, ModelsErrorCode};
use super::super::types::{LoginOptions, ModelAuth, OAuthAuth, OAuthCredential};
use super::device_code::{
    DeviceCodePollOptions, DeviceCodePollResult, poll_oauth_device_code_flow,
};
use crate::generated::get_builtin_models;

/// `CLIENT_ID` — upstream: `decode("SXYxLmI1MDdhMDhjODdlY2ZlOTg=")` (base64/atob).
const CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
/// `COPILOT_HEADERS`.
const COPILOT_HEADERS: [(&str, &str); 4] = [
    ("User-Agent", "GitHubCopilotChat/0.35.0"),
    ("Editor-Version", "vscode/1.107.0"),
    ("Editor-Plugin-Version", "copilot-chat/0.35.0"),
    ("Copilot-Integration-Id", "vscode-chat"),
];
/// `COPILOT_API_VERSION`.
const COPILOT_API_VERSION: &str = "2026-06-01";
/// Default domain when the enterprise prompt is left blank (`|| "github.com"`).
const DEFAULT_DOMAIN: &str = "github.com";
/// `expires = expires_at * 1000 - 5 * 60 * 1000`.
const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;
/// `AbortSignal.timeout(5000)` wrapping every request dispatched through
/// [`GitHubCopilotOAuth::fetch_with_rate_limit_retry`].
const PER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// `grant_type` of the device-code poll (`urn:ietf:params:oauth:grant-type:device_code`).
const DEVICE_CODE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// `{ maxRetries: 2, maxElapsedMs: 5000 }` — the login-time budget shared by
/// the `/models` fetch and the policy-enable POSTs (#6187/#7850).
const LOGIN_RETRY_POLICY: RetryPolicy = RetryPolicy {
    max_retries: 2,
    max_elapsed_ms: 5000,
};

/// `retryPolicy` of `fetchWithRateLimitRetry`.
#[derive(Debug, Clone, Copy)]
struct RetryPolicy {
    max_retries: u32,
    max_elapsed_ms: u64,
}

impl RetryPolicy {
    /// `{ maxRetries: 0, maxElapsedMs: 0 }` — no retry, no deadline; the
    /// first response is final (upstream refresh path).
    const fn zero() -> Self {
        Self {
            max_retries: 0,
            max_elapsed_ms: 0,
        }
    }
}

/// `Date.now()` — milliseconds since the Unix epoch.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn error(message: impl Into<String>) -> ModelsError {
    ModelsError::new(ModelsErrorCode::Oauth, message.into())
}

/// `DeviceCodeResponse`.
#[derive(Debug, Clone)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: Option<f64>,
    expires_in: f64,
}

/// `normalizeDomain` — trim; parse as URL (defaulting to `https://`); keep
/// the hostname. Blank or unparseable input yields `None`.
fn normalize_domain(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let candidate = if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    };
    url::Url::parse(&candidate)
        .ok()
        .map(|url| url.host_str().unwrap_or_default().to_owned())
        .filter(|host| !host.is_empty())
}

/// `getUrls(domain)`.
struct CopilotUrls {
    device_code_url: String,
    access_token_url: String,
    copilot_token_url: String,
}

fn get_urls(domain: &str) -> CopilotUrls {
    CopilotUrls {
        device_code_url: format!("https://{domain}/login/device/code"),
        access_token_url: format!("https://{domain}/login/oauth/access_token"),
        copilot_token_url: format!("https://api.{domain}/copilot_internal/v2/token"),
    }
}

/// `getBaseUrlFromToken` — parse `proxy-ep=proxy.xxx` out of a Copilot token
/// and convert to `https://api.xxx` (`/^proxy\./` → `api.`).
fn get_base_url_from_token(token: &str) -> Option<String> {
    const MARKER: &str = "proxy-ep=";
    let start = token.find(MARKER)? + MARKER.len();
    let rest = &token[start..];
    let end = rest.find(';').unwrap_or(rest.len());
    // `[^;]+` requires at least one non-`;` character.
    if end == 0 {
        return None;
    }
    let proxy_host = &rest[..end];
    let api_host = match proxy_host.strip_prefix("proxy.") {
        Some(rest) => format!("api.{rest}"),
        None => proxy_host.to_owned(),
    };
    Some(format!("https://{api_host}"))
}

/// `getGitHubCopilotBaseUrl` — token `proxy-ep` wins; then the enterprise
/// fallback; then the individual default.
fn get_github_copilot_base_url(token: Option<&str>, enterprise_domain: Option<&str>) -> String {
    if let Some(token) = token
        && let Some(url) = get_base_url_from_token(token)
    {
        return url;
    }
    match enterprise_domain {
        Some(domain) => format!("https://copilot-api.{domain}"),
        None => "https://api.individual.githubcopilot.com".to_owned(),
    }
}

/// One tool-capable `/models` entry (`parseGitHubCopilotModelCatalog`'s
/// `accountModels`).
struct AccountModel {
    id: String,
    picker_enabled: bool,
    policy_state: Option<String>,
}

/// `parseGitHubCopilotModelCatalog` — `data` must be an array ("Invalid
/// Copilot models response"); entries without `tool_calls` support drop out.
/// Returns the selectable catalog (`available_model_ids`: picker-enabled
/// models with policy not disabled, falling back to policy-enabled ids on the
/// Individual endpoint, `14cc26e86` / #7672) plus the policy-enable
/// candidates (`policy_model_ids`: known catalog model ∩ `policy.state ==
/// "unconfigured"` ∩ (`model_picker_enabled` || fallback active),
/// `b3edf0170` / #6187).
#[derive(Debug)]
struct CopilotModelCatalog {
    available_model_ids: Vec<String>,
    policy_model_ids: Vec<String>,
}

fn parse_github_copilot_model_catalog(
    raw: &Value,
    allow_policy_fallback: bool,
) -> Result<CopilotModelCatalog, ModelsError> {
    let data = raw.get("data").and_then(Value::as_array);
    let Some(data) = data else {
        return Err(error("Invalid Copilot models response"));
    };
    let known_model_ids: std::collections::HashSet<&str> = get_builtin_models("github-copilot")
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let mut account_models: Vec<AccountModel> = Vec::new();
    for item in data {
        let tool_calls = item
            .get("capabilities")
            .and_then(|capabilities| capabilities.get("supports"))
            .and_then(|supports| supports.get("tool_calls"))
            .and_then(Value::as_bool);
        if tool_calls == Some(false) {
            continue;
        }
        let Some(id) = item.get("id").and_then(Value::as_str) else {
            continue;
        };
        account_models.push(AccountModel {
            id: id.to_owned(),
            picker_enabled: item.get("model_picker_enabled").and_then(Value::as_bool) == Some(true),
            policy_state: item
                .get("policy")
                .and_then(|policy| policy.get("state"))
                .and_then(Value::as_str)
                .map(str::to_owned),
        });
    }
    let picker_ids: Vec<String> = account_models
        .iter()
        .filter(|model| model.picker_enabled && model.policy_state.as_deref() != Some("disabled"))
        .map(|model| model.id.clone())
        .collect();
    let use_policy_fallback = allow_policy_fallback && picker_ids.is_empty();
    let available_model_ids = if !picker_ids.is_empty() || !allow_policy_fallback {
        picker_ids
    } else {
        account_models
            .iter()
            .filter(|model| model.policy_state.as_deref() == Some("enabled"))
            .map(|model| model.id.clone())
            .collect()
    };
    let policy_model_ids: Vec<String> = account_models
        .iter()
        .filter(|model| {
            model.policy_state.as_deref() == Some("unconfigured")
                && known_model_ids.contains(model.id.as_str())
                && (model.picker_enabled || use_policy_fallback)
        })
        .map(|model| model.id.clone())
        .collect();
    Ok(CopilotModelCatalog {
        available_model_ids,
        policy_model_ids,
    })
}

/// `copilotEnterpriseDomain` — read/normalize the credential's
/// `enterpriseUrl` extra.
fn copilot_enterprise_domain(credential: &OAuthCredential) -> Option<String> {
    let enterprise_url = credential.extra.get("enterpriseUrl")?.as_str()?;
    if enterprise_url.is_empty() {
        return None;
    }
    normalize_domain(enterprise_url)
}

/// `githubCopilotOAuth` — the GitHub Copilot OAuth provider auth.
pub fn github_copilot_oauth() -> Arc<dyn OAuthAuth> {
    Arc::new(GitHubCopilotOAuth::new())
}

/// GitHub Copilot OAuth (`OAuthAuth`) implementation.
pub struct GitHubCopilotOAuth {
    client: reqwest::Client,
    /// Loopback authority for the URL-rewriting test seam (see module docs).
    authority: Option<String>,
}

impl Default for GitHubCopilotOAuth {
    fn default() -> Self {
        Self::new()
    }
}

impl GitHubCopilotOAuth {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
            authority: None,
        }
    }

    /// Test seam — see module docs.
    #[cfg(test)]
    fn with_authority(authority: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            authority: Some(authority.into()),
        }
    }

    /// Apply the test seam: `https://{host}/{path}` →
    /// `http://{authority}/{host}/{path}`. Identity in production.
    fn rewrite(&self, url: String) -> String {
        match &self.authority {
            Some(authority) => match url.strip_prefix("https://") {
                Some(rest) => format!("http://{authority}/{rest}"),
                None => url,
            },
            None => url,
        }
    }

    /// `fetchJson` — send, require a success status
    /// (`{status} {statusText}: {text}` otherwise), parse JSON.
    async fn fetch_json(&self, request: reqwest::RequestBuilder) -> Result<Value, ModelsError> {
        let response = request
            .send()
            .await
            .map_err(|request_error| error(request_error.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let reason = status.canonical_reason().unwrap_or("");
            return Err(error(format!("{} {}: {}", status.as_u16(), reason, text)));
        }
        response
            .json()
            .await
            .map_err(|json_error| error(json_error.to_string()))
    }

    /// `startDeviceFlow` — POST `{domain}/login/device/code`
    /// (`client_id` + `scope=read:user`), validate the response shape and the
    /// `verification_uri` trust boundary (http(s) only; the URI is handed to
    /// the user's browser).
    async fn start_device_flow(&self, domain: &str) -> Result<DeviceCodeResponse, ModelsError> {
        let urls = get_urls(domain);
        let request = self
            .client
            .post(self.rewrite(urls.device_code_url))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::USER_AGENT, "GitHubCopilotChat/0.35.0")
            .form(&[("client_id", CLIENT_ID), ("scope", "read:user")]);
        let data = self.fetch_json(request).await?;

        let Some(object) = data.as_object() else {
            return Err(error("Invalid device code response"));
        };
        let device_code = object.get("device_code").and_then(Value::as_str);
        let user_code = object.get("user_code").and_then(Value::as_str);
        let verification_uri = object.get("verification_uri").and_then(Value::as_str);
        let interval = object.get("interval").and_then(Value::as_f64);
        let expires_in = object.get("expires_in").and_then(Value::as_f64);
        let interval_present_but_not_number = object
            .get("interval")
            .is_some_and(|value| !value.is_number());
        let (Some(device_code), Some(user_code), Some(verification_uri), Some(expires_in)) =
            (device_code, user_code, verification_uri, expires_in)
        else {
            return Err(error("Invalid device code response fields"));
        };
        if interval_present_but_not_number {
            return Err(error("Invalid device code response fields"));
        }

        // The verification URI is opened in the user's browser and to prevent
        // the launcher from opening an executable or similar, we force it to
        // be a URL (and serialize the parsed form back out, like `.href`).
        let parsed_uri = match url::Url::parse(verification_uri) {
            Ok(parsed_uri) => parsed_uri,
            Err(_) => return Err(error("Untrusted verification_uri in device code response")),
        };
        if parsed_uri.scheme() != "https" && parsed_uri.scheme() != "http" {
            return Err(error("Untrusted verification_uri in device code response"));
        }

        Ok(DeviceCodeResponse {
            device_code: device_code.to_owned(),
            user_code: user_code.to_owned(),
            verification_uri: parsed_uri.to_string(),
            interval,
            expires_in,
        })
    }

    /// One access-token poll (`poll` closure of `pollForGitHubAccessToken`):
    /// POST `{domain}/login/oauth/access_token`; map the RFC 8628 error
    /// fields onto the poll result.
    async fn poll_access_token(
        &self,
        domain: &str,
        device: &DeviceCodeResponse,
    ) -> DeviceCodePollResult<String> {
        let urls = get_urls(domain);
        let request = self
            .client
            .post(self.rewrite(urls.access_token_url))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::USER_AGENT, "GitHubCopilotChat/0.35.0")
            .form(&[
                ("client_id", CLIENT_ID),
                ("device_code", device.device_code.as_str()),
                ("grant_type", DEVICE_CODE_GRANT_TYPE),
            ]);
        let raw = match self.fetch_json(request).await {
            Ok(raw) => raw,
            // Upstream lets the fetch error propagate out of the poll closure;
            // the framework surface here is `Failed` (same message text).
            Err(fetch_error) => {
                return DeviceCodePollResult::Failed {
                    message: fetch_error.message,
                };
            }
        };

        if let Some(access_token) = raw.get("access_token").and_then(Value::as_str) {
            return DeviceCodePollResult::Complete {
                value: access_token.to_owned(),
            };
        }

        if let Some(error_code) = raw.get("error").and_then(Value::as_str) {
            let description = raw.get("error_description").and_then(Value::as_str);
            let interval = raw.get("interval").and_then(Value::as_f64);
            match error_code {
                "authorization_pending" => return DeviceCodePollResult::Pending,
                "slow_down" => {
                    return DeviceCodePollResult::SlowDown {
                        interval_seconds: interval,
                    };
                }
                _ => {
                    let suffix = description
                        .map(|description| format!(": {description}"))
                        .unwrap_or_default();
                    return DeviceCodePollResult::Failed {
                        message: format!("Device flow failed: {error_code}{suffix}"),
                    };
                }
            }
        }

        DeviceCodePollResult::Failed {
            message: "Invalid device token response".to_owned(),
        }
    }

    /// `pollForGitHubAccessToken` — `waitBeforeFirstPoll: true`.
    async fn poll_for_github_access_token(
        &self,
        domain: &str,
        device: &DeviceCodeResponse,
        signal: Option<CancellationToken>,
    ) -> Result<String, ModelsError> {
        poll_oauth_device_code_flow(DeviceCodePollOptions {
            interval_seconds: device.interval,
            expires_in_seconds: Some(device.expires_in),
            wait_before_first_poll: true,
            signal,
            poll: || self.poll_access_token(domain, device),
        })
        .await
    }

    /// `refreshGitHubCopilotAccessToken` — GET
    /// `api.{domain}/copilot_internal/v2/token` with the GitHub token as
    /// Bearer; the long-lived GitHub token stays in `refresh`, the Copilot
    /// token becomes `access`.
    async fn refresh_github_copilot_access_token(
        &self,
        refresh_token: &str,
        enterprise_domain: Option<&str>,
    ) -> Result<OAuthCredential, ModelsError> {
        let domain = enterprise_domain.unwrap_or(DEFAULT_DOMAIN);
        let urls = get_urls(domain);
        let mut request = self
            .client
            .get(self.rewrite(urls.copilot_token_url))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {refresh_token}"),
            );
        for (name, value) in COPILOT_HEADERS {
            request = request.header(name, value);
        }
        let raw = self.fetch_json(request).await?;

        let token = raw.get("token").and_then(Value::as_str);
        let expires_at = raw.get("expires_at").and_then(Value::as_f64);
        let (Some(token), Some(expires_at)) = (token, expires_at) else {
            return Err(error("Invalid Copilot token response fields"));
        };

        let mut extra = Map::new();
        if let Some(domain) = enterprise_domain {
            extra.insert("enterpriseUrl".to_owned(), json!(domain));
        }
        Ok(OAuthCredential {
            refresh: refresh_token.to_owned(),
            access: token.to_owned(),
            expires: (expires_at * 1000.0) as i64 - EXPIRY_SKEW_MS,
            extra,
        })
    }

    /// `fetchAvailableGitHubCopilotModelIds` — GET `{baseUrl}/models`
    /// (5s timeout), filtered to the selectable picker catalog. The policy
    /// fallback is limited to the Individual endpoint (some accounts return
    /// false for every picker flag despite explicit enabled policies).
    /// Port of `14cc26e86` (#7672).
    /// `fetchGitHubCopilotModels` — GET `{baseUrl}/models` under the rate
    /// limit retry wrapper, parsed into the availability + policy-enable
    /// catalog (`{status} {statusText}: {text}` on a non-ok response).
    async fn fetch_github_copilot_models(
        &self,
        copilot_token: &str,
        enterprise_domain: Option<&str>,
        signal: Option<&CancellationToken>,
        retry_policy: RetryPolicy,
    ) -> Result<CopilotModelCatalog, ModelsError> {
        let base_url = get_github_copilot_base_url(Some(copilot_token), enterprise_domain);
        // Some Individual accounts return false for every picker flag despite
        // explicit enabled policies. Limit the fallback to that endpoint so
        // other account types keep strict picker semantics.
        let allow_policy_fallback = base_url == "https://api.individual.githubcopilot.com";
        let mut request = self
            .client
            .get(self.rewrite(format!("{base_url}/models")))
            .header(reqwest::header::ACCEPT, "application/json")
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {copilot_token}"),
            )
            .header("X-GitHub-Api-Version", COPILOT_API_VERSION);
        for (name, value) in COPILOT_HEADERS {
            request = request.header(name, value);
        }
        let response = self
            .fetch_with_rate_limit_retry(request, signal, retry_policy)
            .await?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            let reason = status.canonical_reason().unwrap_or("");
            return Err(error(format!("{} {}: {}", status.as_u16(), reason, text)));
        }
        let raw = response
            .json()
            .await
            .map_err(|json_error| error(json_error.to_string()))?;
        parse_github_copilot_model_catalog(&raw, allow_policy_fallback)
    }

    /// `fetchWithRateLimitRetry` (github-copilot.ts:134-166, #6187/#7850):
    /// bounded 429 retry honoring the server `retry-after` header (delay
    /// seconds or an HTTP-date), falling back to exponential backoff
    /// (`500ms * 2^retry`). Each request carries a 5s timeout; the overall
    /// budget (`maxRetries`/`maxElapsedMs`) stops retrying when the next
    /// delay cannot fit before the deadline. A `retry-after` that parses as
    /// neither seconds nor a date returns the response as-is.
    async fn fetch_with_rate_limit_retry(
        &self,
        request: reqwest::RequestBuilder,
        signal: Option<&CancellationToken>,
        retry_policy: RetryPolicy,
    ) -> Result<reqwest::Response, ModelsError> {
        let deadline = if retry_policy.max_retries > 0 && retry_policy.max_elapsed_ms > 0 {
            Some(
                std::time::Instant::now()
                    + std::time::Duration::from_millis(retry_policy.max_elapsed_ms),
            )
        } else {
            None
        };
        for retry in 0u32.. {
            // The builder is consumed per attempt; a non-cloneable body fails
            // the request outright (this flow only sends static bodies).
            let attempt = request
                .try_clone()
                .ok_or_else(|| error("request is not retryable"))?;
            let response = attempt
                .timeout(PER_REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|request_error| error(request_error.to_string()))?;
            if response.status().as_u16() != 429 || retry == retry_policy.max_retries {
                return Ok(response);
            }
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let mut delay_ms: f64 = 500.0 * 2f64.powi(retry as i32);
            if let Some(retry_after) = retry_after {
                match retry_after.trim().parse::<f64>() {
                    Ok(seconds) => delay_ms = seconds * 1000.0,
                    Err(_) => {
                        let Ok(http_date) = httpdate::parse_http_date(retry_after.trim()) else {
                            // `!Number.isFinite(delayMs)` → return the response.
                            return Ok(response);
                        };
                        let target_ms = http_date
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|duration| duration.as_millis() as i64)
                            .unwrap_or(0);
                        delay_ms = (target_ms - now_ms()) as f64;
                    }
                }
            }
            delay_ms = delay_ms.max(0.0);
            if let Some(deadline) = deadline {
                let remaining_ms = deadline
                    .saturating_duration_since(std::time::Instant::now())
                    .as_millis() as f64;
                if delay_ms >= remaining_ms {
                    return Ok(response);
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(delay_ms as u64)).await;
            if signal.is_some_and(|signal| signal.is_cancelled()) {
                return Err(error(super::device_code::CANCEL_MESSAGE));
            }
        }
        unreachable!("retry loop always returns")
    }

    /// `enableGitHubCopilotModel` — POST `{baseUrl}/models/{id}/policy`
    /// (`{state: "enabled"}`, `openai-intent: chat-policy`) under the rate
    /// limit retry wrapper. Network failures are swallowed (`catch → false`)
    /// unless the login was cancelled; a 429 that survives the budget is a
    /// hard error; every other status reports `response.ok`.
    async fn enable_github_copilot_model(
        &self,
        token: &str,
        model_id: &str,
        enterprise_domain: Option<&str>,
        signal: Option<&CancellationToken>,
    ) -> Result<bool, ModelsError> {
        let base_url = get_github_copilot_base_url(Some(token), enterprise_domain);
        let mut request = self
            .client
            .post(self.rewrite(format!("{base_url}/models/{model_id}/policy")))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
            .header("openai-intent", "chat-policy")
            .header("x-interaction-type", "chat-policy")
            .body("{\"state\":\"enabled\"}");
        for (name, value) in COPILOT_HEADERS {
            request = request.header(name, value);
        }
        let response = match self
            .fetch_with_rate_limit_retry(request, signal, LOGIN_RETRY_POLICY)
            .await
        {
            Ok(response) => response,
            Err(request_error) => {
                if signal.is_some_and(|signal| signal.is_cancelled()) {
                    return Err(request_error);
                }
                return Ok(false);
            }
        };
        if response.status().as_u16() == 429 {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let reason = status.canonical_reason().unwrap_or("");
            return Err(error(format!("{} {}: {}", status.as_u16(), reason, text)));
        }
        Ok(response.status().is_success())
    }

    /// `enableGitHubCopilotModels` — enable each policy candidate
    /// **sequentially** (upstream for-loop, #7850): a thrown error (budget
    /// exhausted) stops the loop and keeps the already-enabled ids unless
    /// the login was cancelled.
    async fn enable_github_copilot_models(
        &self,
        token: &str,
        model_ids: &[String],
        enterprise_domain: Option<&str>,
        signal: Option<&CancellationToken>,
    ) -> Result<Vec<String>, ModelsError> {
        let mut enabled_model_ids: Vec<String> = Vec::new();
        for model_id in model_ids {
            match self
                .enable_github_copilot_model(token, model_id, enterprise_domain, signal)
                .await
            {
                Ok(true) => enabled_model_ids.push(model_id.to_owned()),
                Ok(false) => {}
                Err(enable_error) => {
                    if signal.is_some_and(|signal| signal.is_cancelled()) {
                        return Err(enable_error);
                    }
                    break;
                }
            }
        }
        Ok(enabled_model_ids)
    }

    /// `refreshGitHubCopilotToken` — token exchange plus the account's
    /// `availableModelIds`. The models fetch uses a **zero** retry budget
    /// (upstream `{ maxRetries: 0, maxElapsedMs: 0 }`): the first response
    /// is final, 429 included.
    async fn refresh_github_copilot_token(
        &self,
        refresh_token: &str,
        enterprise_domain: Option<&str>,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        let mut credential = self
            .refresh_github_copilot_access_token(refresh_token, enterprise_domain)
            .await?;
        let catalog = self
            .fetch_github_copilot_models(
                &credential.access,
                enterprise_domain,
                signal,
                RetryPolicy::zero(),
            )
            .await?;
        credential.extra.insert(
            "availableModelIds".to_owned(),
            json!(catalog.available_model_ids),
        );
        Ok(credential)
    }

    /// `loginGitHubCopilot`.
    async fn login_github_copilot(
        &self,
        interaction: &dyn AuthInteraction,
    ) -> Result<OAuthCredential, ModelsError> {
        let input = interaction
            .prompt(AuthPrompt::Text {
                message: "GitHub Enterprise URL/domain (blank for github.com)".to_owned(),
                placeholder: Some("company.ghe.com".to_owned()),
                signal: None,
            })
            .await?;
        if interaction
            .signal()
            .is_some_and(|signal| signal.is_cancelled())
        {
            return Err(error(super::device_code::CANCEL_MESSAGE));
        }

        let trimmed = input.trim();
        let enterprise_domain = normalize_domain(&input);
        if !trimmed.is_empty() && enterprise_domain.is_none() {
            return Err(error("Invalid GitHub Enterprise URL/domain"));
        }
        let domain = enterprise_domain.as_deref().unwrap_or(DEFAULT_DOMAIN);

        let device = self.start_device_flow(domain).await?;
        interaction.notify(AuthEvent::DeviceCode {
            user_code: device.user_code.clone(),
            verification_uri: device.verification_uri.clone(),
            interval_seconds: device.interval.map(|interval| interval as u64),
            expires_in_seconds: Some(device.expires_in as u64),
        });

        let github_access_token = self
            .poll_for_github_access_token(domain, &device, interaction.signal())
            .await?;
        let mut credential = self
            .refresh_github_copilot_access_token(&github_access_token, enterprise_domain.as_deref())
            .await?;
        let models = self
            .fetch_github_copilot_models(
                &credential.access,
                enterprise_domain.as_deref(),
                interaction.signal().as_ref(),
                LOGIN_RETRY_POLICY,
            )
            .await?;
        let mut enabled_model_ids: Vec<String> = Vec::new();
        if !models.policy_model_ids.is_empty() {
            interaction.notify(AuthEvent::Progress {
                message: "Enabling models...".to_owned(),
            });
            enabled_model_ids = self
                .enable_github_copilot_models(
                    &credential.access,
                    &models.policy_model_ids,
                    enterprise_domain.as_deref(),
                    interaction.signal().as_ref(),
                )
                .await?;
        }
        // `[...new Set([...availableModelIds, ...enabledModelIds])]` —
        // availability first, freshly enabled ids appended, order-preserving
        // dedup.
        let mut available: Vec<String> = models.available_model_ids;
        for id in enabled_model_ids {
            if !available.contains(&id) {
                available.push(id);
            }
        }
        credential
            .extra
            .insert("availableModelIds".to_owned(), json!(available));
        Ok(credential)
    }
}

#[async_trait::async_trait]
impl OAuthAuth for GitHubCopilotOAuth {
    fn name(&self) -> &str {
        "GitHub Copilot"
    }

    /// `isSubscription: true` (providers/github-copilot.ts:16 @ 4181f66).
    fn is_subscription(&self) -> bool {
        true
    }

    async fn login(
        &self,
        interaction: &dyn AuthInteraction,
        _options: Option<&LoginOptions>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.login_github_copilot(interaction).await
    }

    /// `refresh: (credential) => refreshGitHubCopilotToken(credential.refresh,
    /// copilotEnterpriseDomain(credential))`.
    async fn refresh(
        &self,
        credential: &OAuthCredential,
        signal: Option<&CancellationToken>,
    ) -> Result<OAuthCredential, ModelsError> {
        self.refresh_github_copilot_token(
            &credential.refresh,
            copilot_enterprise_domain(credential).as_deref(),
            signal,
        )
        .await
    }

    /// `toAuth` — derive the credential-specific proxy endpoint per request.
    async fn to_auth(&self, credential: &OAuthCredential) -> Result<ModelAuth, ModelsError> {
        Ok(ModelAuth {
            api_key: Some(credential.access.clone()),
            base_url: Some(get_github_copilot_base_url(
                Some(&credential.access),
                copilot_enterprise_domain(credential).as_deref(),
            )),
            headers: None,
        })
    }
}

#[cfg(test)]
mod tests {
    //! Test intents ported from `packages/ai/test/github-copilot-oauth.test.ts`
    //! @ pi 0.82.1 (2efa728); the mocked `fetch` becomes a loopback axum
    //! server behind the `authority` rewrite seam (module docs), and the
    //! fake-timer interval assertions stay in `device_code.rs`'s fake-clock
    //! tests (the flow-level checks here assert the request/response mapping
    //! with a 1s interval instead).

    use std::sync::Mutex;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::response::Json;
    use serde_json::json;
    use tokio::sync::oneshot;

    use super::super::super::types::BoxFutureSend;
    use super::*;

    // ----- mock GitHub/Copilot endpoints (upstream: `vi.stubGlobal("fetch")`) -----

    #[derive(Debug, Clone)]
    struct RecordedRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl RecordedRequest {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        }
    }

    type Responder = Arc<dyn Fn(&RecordedRequest) -> (StatusCode, Value) + Send + Sync + 'static>;

    struct MockGitHub {
        authority: String,
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        shutdown: Option<oneshot::Sender<()>>,
    }

    struct MockState {
        requests: Arc<Mutex<Vec<RecordedRequest>>>,
        responder: Responder,
    }

    impl MockGitHub {
        async fn start(responder: Responder) -> Self {
            Self::start_with_retry_after(responder, "").await
        }

        /// `start` with a `retry-after` header on every response (429
        /// throttle tests; ignored on 2xx responses).
        async fn start_with_retry_after(responder: Responder, retry_after: &'static str) -> Self {
            let requests: Arc<Mutex<Vec<RecordedRequest>>> = Arc::new(Mutex::new(Vec::new()));
            let state = Arc::new(MockState {
                requests: requests.clone(),
                responder,
            });
            let retry_after = retry_after.to_owned();
            let app = axum::Router::new().fallback(move |request: Request<Body>| {
                let state = state.clone();
                async move {
                    let method = request.method().to_string();
                    let path = request.uri().path().to_owned();
                    let headers = request
                        .headers()
                        .iter()
                        .map(|(name, value)| {
                            (
                                name.to_string(),
                                value.to_str().unwrap_or_default().to_owned(),
                            )
                        })
                        .collect();
                    let body = axum::body::to_bytes(request.into_body(), usize::MAX)
                        .await
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                        .unwrap_or_default();
                    let recorded = RecordedRequest {
                        method,
                        path,
                        headers,
                        body,
                    };
                    state.requests.lock().expect("lock").push(recorded.clone());
                    let (status, body) = (state.responder)(&recorded);
                    let mut response = (status, Json(body)).into_response();
                    if !retry_after.is_empty() {
                        response.headers_mut().insert(
                            "retry-after",
                            retry_after
                                .parse()
                                .expect("static retry-after header value"),
                        );
                    }
                    response
                }
            });
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("addr");
            let (tx, rx) = oneshot::channel::<()>();
            tokio::spawn(async move {
                let _ = axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await;
            });
            Self {
                authority: addr.to_string(),
                requests,
                shutdown: Some(tx),
            }
        }

        fn oauth(&self) -> GitHubCopilotOAuth {
            GitHubCopilotOAuth::with_authority(self.authority.clone())
        }

        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().expect("lock").clone()
        }

        fn requests_matching(&self, needle: &str) -> Vec<RecordedRequest> {
            self.requests()
                .into_iter()
                .filter(|request| request.path.contains(needle))
                .collect()
        }
    }

    impl Drop for MockGitHub {
        fn drop(&mut self) {
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
        }
    }

    // ----- fake AuthInteraction -----

    use axum::response::IntoResponse as _;

    #[derive(Clone, Default)]
    struct InteractionHandle {
        events: Arc<Mutex<Vec<AuthEvent>>>,
        prompts: Arc<Mutex<Vec<AuthPrompt>>>,
    }

    impl InteractionHandle {
        fn events(&self) -> Vec<AuthEvent> {
            self.events.lock().expect("lock").clone()
        }

        fn device_code_event(&self) -> Option<AuthEvent> {
            self.events()
                .into_iter()
                .find(|event| matches!(event, AuthEvent::DeviceCode { .. }))
        }
    }

    struct FakeInteraction {
        handle: InteractionHandle,
        answer: String,
    }

    impl FakeInteraction {
        fn new(handle: InteractionHandle, answer: &str) -> Self {
            Self {
                handle,
                answer: answer.to_owned(),
            }
        }
    }

    impl AuthInteraction for FakeInteraction {
        fn prompt<'a>(
            &'a self,
            prompt: AuthPrompt,
        ) -> BoxFutureSend<'a, Result<String, ModelsError>> {
            self.handle.prompts.lock().expect("lock").push(prompt);
            let answer = self.answer.clone();
            Box::pin(async move { Ok(answer) })
        }

        fn notify(&self, event: AuthEvent) {
            self.handle.events.lock().expect("lock").push(event);
        }
    }

    // ----- canned upstream response bodies -----

    fn device_code_response() -> Value {
        json!({
            "device_code": "device-code",
            "user_code": "ABCD-EFGH",
            "verification_uri": "https://github.com/login/device",
            "interval": 1,
            "expires_in": 900,
        })
    }

    const COPILOT_TOKEN: &str =
        "tid=test;exp=9999999999;proxy-ep=proxy.individual.githubcopilot.com;";

    fn copilot_token_response() -> Value {
        json!({
            "token": COPILOT_TOKEN,
            "expires_at": 9999999999_i64,
        })
    }

    /// Upstream picker payload (github-copilot-oauth.test.ts:77-96): only
    /// `gpt-4.1` is selectable.
    fn picker_models_response() -> Value {
        json!({
            "data": [
                {
                    "id": "gpt-4.1",
                    "model_picker_enabled": true,
                    "capabilities": { "supports": { "tool_calls": true } },
                },
                {
                    "id": "claude-opus-4.7",
                    "model_picker_enabled": true,
                    "policy": { "state": "disabled" },
                    "capabilities": { "supports": { "tool_calls": true } },
                },
                {
                    "id": "gpt-5.4-nano",
                    "model_picker_enabled": false,
                    "capabilities": { "supports": { "tool_calls": true } },
                },
            ]
        })
    }

    /// Happy-path responder: device code → access token → Copilot token →
    /// policy POSTs → models GET.
    fn happy_path_responder() -> Responder {
        Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (StatusCode::OK, device_code_response());
            }
            if path.ends_with("/login/oauth/access_token") {
                return (
                    StatusCode::OK,
                    json!({ "access_token": "ghu_refresh_token" }),
                );
            }
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/policy") {
                return (StatusCode::OK, Value::Null);
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, picker_models_response());
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        })
    }

    // ----- pure-function tests -----

    /// `normalizeDomain` branches.
    #[test]
    fn normalize_domain_cases() {
        assert_eq!(normalize_domain(""), None);
        assert_eq!(normalize_domain("   "), None);
        assert_eq!(
            normalize_domain("company.ghe.com"),
            Some("company.ghe.com".to_owned())
        );
        assert_eq!(
            normalize_domain(" https://company.ghe.com/some/path?q=1 "),
            Some("company.ghe.com".to_owned())
        );
        assert_eq!(normalize_domain("http://[bad"), None);
        assert_eq!(normalize_domain("not a url"), None);
    }

    /// `getUrls` for the default and enterprise domains.
    #[test]
    fn get_urls_maps_domains() {
        let urls = get_urls("github.com");
        assert_eq!(urls.device_code_url, "https://github.com/login/device/code");
        assert_eq!(
            urls.access_token_url,
            "https://github.com/login/oauth/access_token"
        );
        assert_eq!(
            urls.copilot_token_url,
            "https://api.github.com/copilot_internal/v2/token"
        );
        let urls = get_urls("company.ghe.com");
        assert_eq!(
            urls.device_code_url,
            "https://company.ghe.com/login/device/code"
        );
        assert_eq!(
            urls.copilot_token_url,
            "https://api.company.ghe.com/copilot_internal/v2/token"
        );
    }

    /// `getBaseUrlFromToken`: `proxy-ep` parse + `proxy.` → `api.` conversion.
    #[test]
    fn base_url_from_token_cases() {
        assert_eq!(
            get_base_url_from_token(COPILOT_TOKEN),
            Some("https://api.individual.githubcopilot.com".to_owned())
        );
        assert_eq!(get_base_url_from_token("tid=x;exp=1;"), None);
        // `[^;]+` requires a non-empty host.
        assert_eq!(get_base_url_from_token("proxy-ep=;tid=x"), None);
        // Only a leading `proxy.` is replaced.
        assert_eq!(
            get_base_url_from_token("proxy-ep=proxy.proxy.example.com;"),
            Some("https://api.proxy.example.com".to_owned())
        );
        assert_eq!(
            get_base_url_from_token("proxy-ep=other.example.com;"),
            Some("https://other.example.com".to_owned())
        );
    }

    /// `getGitHubCopilotBaseUrl` precedence: token → enterprise → default.
    #[test]
    fn base_url_precedence() {
        assert_eq!(
            get_github_copilot_base_url(Some(COPILOT_TOKEN), Some("company.ghe.com")),
            "https://api.individual.githubcopilot.com"
        );
        assert_eq!(
            get_github_copilot_base_url(Some("no-proxy-ep"), Some("company.ghe.com")),
            "https://copilot-api.company.ghe.com"
        );
        assert_eq!(
            get_github_copilot_base_url(None, None),
            "https://api.individual.githubcopilot.com"
        );
        assert_eq!(
            get_github_copilot_base_url(Some("no-proxy-ep"), None),
            "https://api.individual.githubcopilot.com"
        );
    }

    /// `parseGitHubCopilotModelCatalog` selection rules, the policy fallback,
    /// the policy-enable candidates, and the non-array error. Port of
    /// `14cc26e86` (#7672) + `b3edf0170` (#6187).
    #[test]
    fn parse_model_catalog_filters() {
        // Default: picker wins (gpt-4.1 is the only picker-enabled model
        // with policy not disabled); no unconfigured known models → no
        // policy-enable candidates.
        let catalog =
            parse_github_copilot_model_catalog(&picker_models_response(), false).expect("catalog");
        assert_eq!(catalog.available_model_ids, vec!["gpt-4.1".to_owned()]);
        assert!(catalog.policy_model_ids.is_empty());

        // tool_calls explicitly unsupported drops the model entirely (both
        // lists); entries without an id drop out.
        let catalog = parse_github_copilot_model_catalog(
            &json!({
                "data": [
                    { "id": "no-tools", "model_picker_enabled": true,
                      "capabilities": { "supports": { "tool_calls": false } } },
                    { "id": "kept", "model_picker_enabled": true },
                    { "model_picker_enabled": true },
                ]
            }),
            false,
        )
        .expect("catalog");
        assert_eq!(catalog.available_model_ids, vec!["kept".to_owned()]);

        let error = parse_github_copilot_model_catalog(&json!({ "data": {} }), false)
            .expect_err("non-array data");
        assert_eq!(error.message, "Invalid Copilot models response");
    }

    /// Policy-enable candidates: known catalog model ∩ `policy.state ==
    /// "unconfigured"` ∩ (`model_picker_enabled` || fallback active).
    ///
    /// Catalog-sensitive fixture ids are derived from the vendored catalog
    /// (upstream ef41a24b3 does the same for Copilot tests) so the test does
    /// not break when models.dev retires models from the catalog (V14-09:
    /// claude-sonnet-4 / -4.5 disappeared from the regenerated catalog).
    #[test]
    fn parse_model_catalog_policy_enable_candidates() {
        let known: Vec<&str> = get_builtin_models("github-copilot")
            .iter()
            .map(|model| model.id.as_str())
            .collect();
        let (candidate, fallback_candidate, configured) = (known[0], known[1], known[2]);
        let catalog = parse_github_copilot_model_catalog(
            &json!({
                "data": [
                    // Known + unconfigured + picker: candidate.
                    { "id": candidate, "model_picker_enabled": true,
                      "policy": { "state": "unconfigured" } },
                    // Known + unconfigured but not picker-enabled: not a
                    // candidate while any picker model exists.
                    { "id": fallback_candidate, "model_picker_enabled": false,
                      "policy": { "state": "unconfigured" } },
                    // Unconfigured + picker but not in the known catalog.
                    { "id": "custom-model", "model_picker_enabled": true,
                      "policy": { "state": "unconfigured" } },
                    // Known + picker but already configured.
                    { "id": configured, "model_picker_enabled": true,
                      "policy": { "state": "enabled" } },
                ]
            }),
            false,
        )
        .expect("catalog");
        assert_eq!(catalog.policy_model_ids, vec![candidate.to_owned()]);
        // Availability has no known-catalog filter (only policy candidates
        // do): the unknown picker model stays selectable.
        assert_eq!(
            catalog.available_model_ids,
            vec![
                candidate.to_owned(),
                "custom-model".to_owned(),
                configured.to_owned()
            ]
        );

        // Fallback active (Individual endpoint, zero picker models): the
        // non-picker unconfigured known model becomes a candidate too.
        let catalog = parse_github_copilot_model_catalog(
            &json!({
                "data": [
                    { "id": fallback_candidate, "model_picker_enabled": false,
                      "policy": { "state": "unconfigured" } },
                ]
            }),
            true,
        )
        .expect("catalog");
        assert_eq!(
            catalog.policy_model_ids,
            vec![fallback_candidate.to_owned()]
        );
        assert!(catalog.available_model_ids.is_empty());
    }

    /// All picker flags false on Individual endpoint → policy fallback returns
    /// policy-enabled models (14cc26e86).
    #[test]
    fn parse_model_catalog_policy_fallback_individual() {
        let catalog = parse_github_copilot_model_catalog(
            &json!({
                "data": [
                    { "id": "gpt-5", "model_picker_enabled": false,
                      "policy": { "state": "enabled" } },
                    { "id": "claude", "model_picker_enabled": false,
                      "policy": { "state": "enabled" } },
                    { "id": "disabled-model", "model_picker_enabled": false,
                      "policy": { "state": "disabled" } },
                ]
            }),
            true, // allow_policy_fallback (Individual endpoint)
        )
        .expect("catalog");
        assert_eq!(
            catalog.available_model_ids,
            vec!["gpt-5".to_owned(), "claude".to_owned()]
        );
    }

    /// All picker flags false on enterprise endpoint → strict (empty result),
    /// even with enabled policies.
    #[test]
    fn parse_model_catalog_no_fallback_enterprise() {
        let catalog = parse_github_copilot_model_catalog(
            &json!({
                "data": [
                    { "id": "gpt-5", "model_picker_enabled": false,
                      "policy": { "state": "enabled" } },
                ]
            }),
            false, // no fallback (enterprise endpoint)
        )
        .expect("catalog");
        assert!(catalog.available_model_ids.is_empty());
    }

    /// Mixed picker → picker wins even with policy enabled.
    #[test]
    fn parse_model_catalog_picker_wins_over_policy() {
        let catalog = parse_github_copilot_model_catalog(
            &json!({
                "data": [
                    { "id": "picker-model", "model_picker_enabled": true,
                      "policy": { "state": "enabled" } },
                    { "id": "policy-only-model", "model_picker_enabled": false,
                      "policy": { "state": "enabled" } },
                ]
            }),
            true,
        )
        .expect("catalog");
        assert_eq!(catalog.available_model_ids, vec!["picker-model".to_owned()]);
    }

    // ----- flow tests against the mock -----

    /// `reports device-code details through onDeviceCode` + happy-path login:
    /// device-code event, policy-enable for every catalog model, models fetch,
    /// credential shape.
    #[tokio::test]
    async fn login_happy_path_reports_device_code_and_enables_models() {
        let mock = MockGitHub::start(happy_path_responder()).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone(), "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.refresh, "ghu_refresh_token");
        assert_eq!(credential.access, COPILOT_TOKEN);
        assert_eq!(credential.expires, 9999999999_i64 * 1000 - EXPIRY_SKEW_MS);
        assert_eq!(
            credential.extra.get("availableModelIds"),
            Some(&json!(["gpt-4.1"]))
        );
        assert!(credential.extra.get("enterpriseUrl").is_none());

        // Prompt shape (github-copilot.ts:330-334).
        let prompts = handle.prompts.lock().expect("lock");
        assert_eq!(prompts.len(), 1);
        match &prompts[0] {
            AuthPrompt::Text {
                message,
                placeholder,
                ..
            } => {
                assert_eq!(
                    message,
                    "GitHub Enterprise URL/domain (blank for github.com)"
                );
                assert_eq!(placeholder.as_deref(), Some("company.ghe.com"));
            }
            other => panic!("expected text prompt, got {other:?}"),
        }
        drop(prompts);

        // Device-code event (upstream `onDeviceCode` assertion).
        match handle.device_code_event().expect("device_code event") {
            AuthEvent::DeviceCode {
                user_code,
                verification_uri,
                interval_seconds,
                expires_in_seconds,
            } => {
                assert_eq!(user_code, "ABCD-EFGH");
                assert_eq!(verification_uri, "https://github.com/login/device");
                assert_eq!(interval_seconds, Some(1));
                assert_eq!(expires_in_seconds, Some(900));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        // No unconfigured known models in the payload → the enable step is
        // skipped entirely: no "Enabling models..." event, no policy POSTs
        // (#6187/#7850 sequence: fetch once, enable conditionally).
        assert!(!handle.events().iter().any(|event| matches!(
            event,
            AuthEvent::Progress { message } if message == "Enabling models..."
        )));

        let requests = mock.requests();
        // Device code request (github.com default domain via the seam path).
        let device = &requests[0];
        assert_eq!(device.method, "POST");
        assert_eq!(device.path, "/github.com/login/device/code");
        assert_eq!(device.header("accept"), Some("application/json"));
        assert_eq!(
            device.header("content-type"),
            Some("application/x-www-form-urlencoded")
        );
        assert!(device.body.contains("client_id="));
        assert!(device.body.contains("scope=read%3Auser"));

        // Access-token poll.
        let poll = &requests[1];
        assert_eq!(poll.path, "/github.com/login/oauth/access_token");
        assert!(poll.body.contains("device_code=device-code"));
        assert!(
            poll.body
                .contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code")
        );

        // Copilot token exchange: GitHub token as Bearer + Copilot headers.
        let exchange = &requests[2];
        assert_eq!(exchange.method, "GET");
        assert_eq!(exchange.path, "/api.github.com/copilot_internal/v2/token");
        assert_eq!(
            exchange.header("authorization"),
            Some("Bearer ghu_refresh_token")
        );
        assert_eq!(
            exchange.header("user-agent"),
            Some("GitHubCopilotChat/0.35.0")
        );
        assert_eq!(
            exchange.header("copilot-integration-id"),
            Some("vscode-chat")
        );

        // No policy POSTs at all (the payload has no unconfigured known
        // models; the old enable-everything behavior is gone).
        assert!(mock.requests_matching("/policy").is_empty());

        // The models fetch is last and carries the API version header.
        let last = requests.last().expect("models request");
        assert_eq!(last.method, "GET");
        assert_eq!(last.path, "/api.individual.githubcopilot.com/models");
        assert_eq!(last.header("x-github-api-version"), Some("2026-06-01"));
        assert_eq!(
            last.header("authorization"),
            Some(format!("Bearer {COPILOT_TOKEN}").as_str())
        );
    }

    /// Unconfigured known models are enabled **sequentially** before the
    /// credential is finalized; freshly enabled ids join `availableModelIds`
    /// (`b3edf0170` / `d5278eaac`, #6187/#7850). The unconfigured fixture id
    /// is derived from the vendored catalog (upstream ef41a24b3 pattern;
    /// claude-sonnet-4.5 was retired from the catalog by the V14-09 regen).
    #[tokio::test]
    async fn login_enables_unconfigured_known_models_sequentially() {
        let unconfigured: String = get_builtin_models("github-copilot")[0].id.clone();
        let models_response = json!({
            "data": [
                { "id": "gpt-4.1", "model_picker_enabled": true },
                { "id": unconfigured, "model_picker_enabled": true,
                  "policy": { "state": "unconfigured" } },
                { "id": "gpt-5.4", "model_picker_enabled": true,
                  "policy": { "state": "unconfigured" } },
            ]
        });
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (StatusCode::OK, device_code_response());
            }
            if path.ends_with("/login/oauth/access_token") {
                return (
                    StatusCode::OK,
                    json!({ "access_token": "ghu_refresh_token" }),
                );
            }
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/policy") {
                return (StatusCode::OK, Value::Null);
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, models_response.clone());
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone(), "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert!(handle.events().iter().any(|event| matches!(
            event,
            AuthEvent::Progress { message } if message == "Enabling models..."
        )));
        // Exactly the unconfigured known models, in payload order.
        let policies = mock.requests_matching("/policy");
        let policy_paths: Vec<&str> = policies
            .iter()
            .map(|request| request.path.as_str())
            .collect();
        assert_eq!(
            policy_paths,
            vec![
                format!("/api.individual.githubcopilot.com/models/{unconfigured}/policy"),
                "/api.individual.githubcopilot.com/models/gpt-5.4/policy".to_owned(),
            ]
        );
        for policy in &policies {
            assert_eq!(policy.method, "POST");
            assert_eq!(policy.header("openai-intent"), Some("chat-policy"));
            assert_eq!(policy.body, "{\"state\":\"enabled\"}");
        }
        // Availability first, freshly enabled ids appended.
        assert_eq!(
            credential.extra.get("availableModelIds"),
            Some(&json!(["gpt-4.1", unconfigured, "gpt-5.4"]))
        );
    }

    /// A policy POST throttled with 429 is retried inside the login budget
    /// and honors `retry-after` (seconds form) before succeeding (#7850).
    #[tokio::test]
    async fn login_policy_post_retries_429_honoring_retry_after() {
        let models_response = json!({
            "data": [
                { "id": "gpt-5.4", "model_picker_enabled": true,
                  "policy": { "state": "unconfigured" } },
            ]
        });
        let throttled = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (StatusCode::OK, device_code_response());
            }
            if path.ends_with("/login/oauth/access_token") {
                return (StatusCode::OK, json!({ "access_token": "ghu" }));
            }
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/policy") {
                if throttled.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                    return (StatusCode::TOO_MANY_REQUESTS, Value::Null);
                }
                return (StatusCode::OK, Value::Null);
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, models_response.clone());
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start_with_retry_after(responder, "0").await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle, "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(
            credential.extra.get("availableModelIds"),
            Some(&json!(["gpt-5.4"]))
        );
        assert_eq!(mock.requests_matching("/policy").len(), 2);
    }

    /// A 429 that survives the retry budget (`maxRetries: 2`) stops the
    /// enable loop (upstream catch → `break`), keeping earlier progress;
    /// the login itself still succeeds.
    #[tokio::test]
    async fn login_policy_post_429_past_budget_stops_enable_loop() {
        let models_response = json!({
            "data": [
                { "id": "gpt-5.4", "model_picker_enabled": true,
                  "policy": { "state": "unconfigured" } },
            ]
        });
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (StatusCode::OK, device_code_response());
            }
            if path.ends_with("/login/oauth/access_token") {
                return (StatusCode::OK, json!({ "access_token": "ghu" }));
            }
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/policy") {
                return (StatusCode::TOO_MANY_REQUESTS, Value::Null);
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, models_response.clone());
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start_with_retry_after(responder, "0").await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle, "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        // Initial attempt + 2 retries, then the loop breaks.
        assert_eq!(mock.requests_matching("/policy").len(), 3);
        // No model was enabled, but the fetched availability survives.
        assert_eq!(
            credential.extra.get("availableModelIds"),
            Some(&json!(["gpt-5.4"]))
        );
    }

    /// Refresh uses a zero retry budget: the first 429 is final (upstream
    /// `{ maxRetries: 0, maxElapsedMs: 0 }`).
    #[tokio::test]
    async fn refresh_models_fetch_does_not_retry_429() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/models") {
                return (StatusCode::TOO_MANY_REQUESTS, Value::Null);
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start_with_retry_after(responder, "0").await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "ghu_refresh_token".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let error = oauth
            .refresh(&credential, None)
            .await
            .expect_err("first 429 is final");
        assert!(error.message.starts_with("429"), "error: {}", error.message);
        assert_eq!(mock.requests_matching("/models").len(), 1);
    }

    /// Enterprise domain: the prompt answer is normalized, every endpoint
    /// moves to the enterprise host, and `enterpriseUrl` rides the credential.
    #[tokio::test]
    async fn login_with_enterprise_domain_targets_enterprise_endpoints() {
        // Token without a proxy-ep → the enterprise `copilot-api.` fallback
        // serves /models and the policy POSTs.
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (StatusCode::OK, device_code_response());
            }
            if path.ends_with("/login/oauth/access_token") {
                return (StatusCode::OK, json!({ "access_token": "ghu_enterprise" }));
            }
            if path.contains("/copilot_internal/v2/token") {
                return (
                    StatusCode::OK,
                    json!({ "token": "tid=x;exp=1;", "expires_at": 9999999999_i64 }),
                );
            }
            if path.ends_with("/policy") {
                return (StatusCode::OK, Value::Null);
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, json!({ "data": [] }));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle, "https://company.ghe.com/");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(
            credential.extra.get("enterpriseUrl"),
            Some(&json!("company.ghe.com"))
        );

        let requests = mock.requests();
        assert_eq!(requests[0].path, "/company.ghe.com/login/device/code");
        assert_eq!(
            requests[1].path,
            "/company.ghe.com/login/oauth/access_token"
        );
        assert_eq!(
            requests[2].path,
            "/api.company.ghe.com/copilot_internal/v2/token"
        );
        let last = requests.last().expect("models request");
        assert_eq!(last.path, "/copilot-api.company.ghe.com/models");
        // Empty catalog → no policy-enable candidates → no POSTs.
        assert!(mock.requests_matching("/policy").is_empty());
    }

    /// `rejects a non-http(s) verification_uri before it reaches onDeviceCode`.
    #[tokio::test]
    async fn login_rejects_untrusted_verification_uri() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            assert!(request.path.ends_with("/login/device/code"));
            (
                StatusCode::OK,
                json!({
                    "device_code": "device-code",
                    "user_code": "ABCD-EFGH",
                    "verification_uri": "$(id>/tmp/pwned)",
                    "interval": 1,
                    "expires_in": 900,
                }),
            )
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone(), "");

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("untrusted uri");
        assert_eq!(
            error.message,
            "Untrusted verification_uri in device code response"
        );
        assert!(handle.device_code_event().is_none());
    }

    /// `normalizes verification_uri before it reaches onDeviceCode`.
    #[tokio::test]
    async fn login_normalizes_verification_uri() {
        let raw = "https://github.com/login/\u{1b}]8;;evil";
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (
                    StatusCode::OK,
                    json!({
                        "device_code": "device-code",
                        "user_code": "ABCD-EFGH",
                        "verification_uri": raw,
                        "interval": 1,
                        "expires_in": 900,
                    }),
                );
            }
            if path.ends_with("/login/oauth/access_token") {
                return (
                    StatusCode::OK,
                    json!({ "access_token": "ghu_refresh_token" }),
                );
            }
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/policy") {
                return (StatusCode::OK, Value::Null);
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, json!({ "data": [] }));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle.clone(), "");

        oauth.login(&interaction, None).await.expect("login");
        match handle.device_code_event().expect("device_code event") {
            AuthEvent::DeviceCode {
                verification_uri, ..
            } => {
                assert_ne!(verification_uri, raw);
                assert_eq!(verification_uri, "https://github.com/login/%1B]8;;evil");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    /// A non-empty enterprise input that fails to parse is rejected verbatim.
    #[tokio::test]
    async fn login_rejects_invalid_enterprise_domain() {
        let mock = MockGitHub::start(happy_path_responder()).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle, "http://[bad");

        let error = oauth
            .login(&interaction, None)
            .await
            .expect_err("invalid domain");
        assert_eq!(error.message, "Invalid GitHub Enterprise URL/domain");
        assert!(mock.requests().is_empty(), "no request may leave");
    }

    /// Poll mapping: `authorization_pending` → pending, `slow_down` with a
    /// server interval → honored backoff, then completion. (Interval timing
    /// itself is pinned by `device_code.rs`'s fake-clock tests.)
    #[tokio::test]
    async fn poll_handles_pending_slow_down_then_completes() {
        let polls = Arc::new(Mutex::new(0usize));
        let responder: Responder = Arc::new(move |request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (StatusCode::OK, device_code_response());
            }
            if path.ends_with("/login/oauth/access_token") {
                let mut count = polls.lock().expect("lock");
                *count += 1;
                return match *count {
                    1 => (
                        StatusCode::OK,
                        json!({ "error": "authorization_pending", "error_description": "pending" }),
                    ),
                    2 => (
                        StatusCode::OK,
                        json!({ "error": "slow_down", "error_description": "slow down", "interval": 1 }),
                    ),
                    _ => (
                        StatusCode::OK,
                        json!({ "access_token": "ghu_refresh_token" }),
                    ),
                };
            }
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/policy") {
                return (StatusCode::OK, Value::Null);
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, json!({ "data": [] }));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle, "");

        let credential = oauth.login(&interaction, None).await.expect("login");
        assert_eq!(credential.refresh, "ghu_refresh_token");
        let poll_count = mock.requests_matching("/login/oauth/access_token").len();
        assert_eq!(poll_count, 3);
    }

    /// Unknown device-flow errors surface as `Device flow failed: …`.
    #[tokio::test]
    async fn poll_unknown_error_maps_to_failed_message() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.ends_with("/login/device/code") {
                return (StatusCode::OK, device_code_response());
            }
            if path.ends_with("/login/oauth/access_token") {
                return (
                    StatusCode::OK,
                    json!({ "error": "access_denied", "error_description": "denied" }),
                );
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let handle = InteractionHandle::default();
        let interaction = FakeInteraction::new(handle, "");

        let error = oauth.login(&interaction, None).await.expect_err("denied");
        assert_eq!(error.message, "Device flow failed: access_denied: denied");
    }

    /// `filters models to the authenticated account picker catalog` — the
    /// refresh half: token exchange + picker fetch; the catalog narrowing
    /// through the factory's `filter_models` is covered end to end in
    /// `tests/oauth_copilot_radius.rs`.
    #[tokio::test]
    async fn refresh_exchanges_token_and_fetches_available_model_ids() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.contains("/copilot_internal/v2/token") {
                assert_eq!(
                    request.header("authorization"),
                    Some("Bearer ghu_refresh_token")
                );
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/models") {
                assert_eq!(
                    request.header("authorization"),
                    Some(format!("Bearer {COPILOT_TOKEN}").as_str())
                );
                return (StatusCode::OK, picker_models_response());
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let credential = OAuthCredential {
            refresh: "ghu_refresh_token".to_owned(),
            access: "old-access-token".to_owned(),
            expires: 0,
            extra: Map::new(),
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(refreshed.refresh, "ghu_refresh_token");
        assert_eq!(refreshed.access, COPILOT_TOKEN);
        assert_eq!(refreshed.expires, 9999999999_i64 * 1000 - EXPIRY_SKEW_MS);
        assert_eq!(
            refreshed.extra.get("availableModelIds"),
            Some(&json!(["gpt-4.1"]))
        );
        let requests = mock.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].path,
            "/api.github.com/copilot_internal/v2/token"
        );
        assert_eq!(requests[1].path, "/api.individual.githubcopilot.com/models");
    }

    /// Enterprise refresh: `enterpriseUrl` on the credential retargets the
    /// token exchange to `api.{domain}`.
    #[tokio::test]
    async fn refresh_with_enterprise_url_uses_enterprise_domain() {
        let responder: Responder = Arc::new(|request: &RecordedRequest| {
            let path = request.path.as_str();
            if path.contains("/copilot_internal/v2/token") {
                return (StatusCode::OK, copilot_token_response());
            }
            if path.ends_with("/models") {
                return (StatusCode::OK, json!({ "data": [] }));
            }
            panic!("unexpected request: {} {}", request.method, request.path);
        });
        let mock = MockGitHub::start(responder).await;
        let oauth = mock.oauth();
        let mut extra = Map::new();
        extra.insert("enterpriseUrl".to_owned(), json!("company.ghe.com"));
        let credential = OAuthCredential {
            refresh: "ghu_refresh_token".to_owned(),
            access: "old".to_owned(),
            expires: 0,
            extra,
        };

        let refreshed = oauth.refresh(&credential, None).await.expect("refresh");
        assert_eq!(
            refreshed.extra.get("enterpriseUrl"),
            Some(&json!("company.ghe.com"))
        );
        let requests = mock.requests();
        assert_eq!(
            requests[0].path,
            "/api.company.ghe.com/copilot_internal/v2/token"
        );
    }

    /// `toAuth` derives the per-account base URL (token `proxy-ep` →
    /// enterprise fallback → individual default).
    #[tokio::test]
    async fn to_auth_derives_per_account_base_url() {
        let oauth = GitHubCopilotOAuth::new();
        let credential = |access: &str, enterprise: Option<&str>| {
            let mut extra = Map::new();
            if let Some(domain) = enterprise {
                extra.insert("enterpriseUrl".to_owned(), json!(domain));
            }
            OAuthCredential {
                refresh: "r".to_owned(),
                access: access.to_owned(),
                expires: i64::MAX,
                extra,
            }
        };

        let auth = oauth
            .to_auth(&credential(COPILOT_TOKEN, None))
            .await
            .expect("to_auth");
        assert_eq!(auth.api_key.as_deref(), Some(COPILOT_TOKEN));
        assert_eq!(
            auth.base_url.as_deref(),
            Some("https://api.individual.githubcopilot.com")
        );

        let auth = oauth
            .to_auth(&credential("no-proxy-ep", Some("company.ghe.com")))
            .await
            .expect("to_auth");
        assert_eq!(
            auth.base_url.as_deref(),
            Some("https://copilot-api.company.ghe.com")
        );

        let auth = oauth
            .to_auth(&credential("no-proxy-ep", None))
            .await
            .expect("to_auth");
        assert_eq!(
            auth.base_url.as_deref(),
            Some("https://api.individual.githubcopilot.com")
        );
    }
}
