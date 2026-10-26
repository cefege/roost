//! Port of `packages/ai/src/providers/radius-config.ts` @ pi 0.82.1 (2efa728)
//! — Radius gateway config: URL normalization, the sanitized
//! `RadiusGatewayConfig` shape (stored on the OAuth credential and fetched
//! from `{gateway}/v1/config`), and model materialization onto the
//! `pi-messages` API.
//!
//! Intentional differences:
//! - The upstream runtime type guards (`isRadiusGatewayModel` /
//!   `sanitizeRadiusGatewayConfig`) become serde deserialization after the
//!   same shape pre-checks; values failing serde are dropped like upstream's
//!   `filter` (upstream spreads unvalidated fields through an unchecked cast
//!   — Rust materializes straight into [`Model`], so unrepresentable extra
//!   fields are simply not carried).
//! - `truncateHttpBody` counts Unicode scalar values, not UTF-16 code units
//!   (same precedent as D-021).

use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::auth::{ModelsError, ModelsErrorCode, OAuthCredential};
use crate::types::{ApiKind, InputModality, Model, ModelCost, ThinkingLevelMap};

/// `DEFAULT_RADIUS_GATEWAY`.
pub const DEFAULT_RADIUS_GATEWAY: &str = "https://radius.pi.dev";

/// `RadiusGatewayModel` — the gateway-reported model shape (everything but
/// the pi-side `api`/`provider`/`baseUrl`, which materialization supplies).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadiusGatewayModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    #[serde(default)]
    pub thinking_level_map: Option<ThinkingLevelMap>,
    pub input: Vec<InputModality>,
    pub cost: ModelCost,
    pub context_window: u32,
    pub max_tokens: u32,
}

/// `RadiusGatewayConfig`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RadiusGatewayConfig {
    pub base_url: String,
    pub models: Vec<RadiusGatewayModel>,
}

/// `isRadiusGatewayModel` — shape pre-check mirroring the upstream guard,
/// then serde for the typed fields (see module docs).
fn sanitize_radius_gateway_model(value: &Value) -> Option<RadiusGatewayModel> {
    let object = value.as_object()?;
    if !object.get("id").is_some_and(Value::is_string)
        || !object.get("name").is_some_and(Value::is_string)
        || !object.get("reasoning").is_some_and(Value::is_boolean)
        || !object.get("input").is_some_and(Value::is_array)
        || !object.get("cost").is_some_and(Value::is_object)
        || !object.get("contextWindow").is_some_and(Value::is_number)
        || !object.get("maxTokens").is_some_and(Value::is_number)
    {
        return None;
    }
    serde_json::from_value(value.clone()).ok()
}

/// `sanitizeRadiusGatewayConfig` — `None` unless the value is an object with
/// a string `baseUrl` and an array `models`; invalid model entries are
/// filtered out.
pub fn sanitize_radius_gateway_config(value: &Value) -> Option<RadiusGatewayConfig> {
    let object = value.as_object()?;
    let base_url = object.get("baseUrl")?.as_str()?.to_owned();
    let models = object.get("models")?.as_array()?;
    Some(RadiusGatewayConfig {
        base_url,
        models: models
            .iter()
            .filter_map(sanitize_radius_gateway_model)
            .collect(),
    })
}

/// `normalizeRadiusGatewayUrl` — default to `https://`, strip trailing
/// slashes.
pub fn normalize_radius_gateway_url(value: &str) -> String {
    let has_scheme = value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"));
    let with_scheme = if has_scheme {
        value.to_owned()
    } else {
        format!("https://{value}")
    };
    with_scheme.trim_end_matches('/').to_owned()
}

/// `getRadiusCredentialConfig` — the sanitized `gatewayConfig` extra of an
/// OAuth credential (`RadiusOAuthCredential`).
pub fn get_radius_credential_config(
    credential: Option<&OAuthCredential>,
) -> Option<RadiusGatewayConfig> {
    sanitize_radius_gateway_config(credential?.extra.get("gatewayConfig")?)
}

/// `getRadiusModelsFromConfig` — materialize gateway models onto
/// `pi-messages` for `provider_id`, served from `config.base_url`.
pub fn get_radius_models_from_config(
    provider_id: &str,
    config: &RadiusGatewayConfig,
) -> Vec<Model> {
    config
        .models
        .iter()
        .map(|model| Model {
            id: model.id.clone(),
            name: model.name.clone(),
            api: ApiKind::from(ApiKind::RPI_MESSAGES),
            provider: provider_id.to_owned(),
            base_url: config.base_url.clone(),
            reasoning: model.reasoning,
            thinking_level_map: model.thinking_level_map.clone(),
            input: model.input.clone(),
            input_limits: None,
            cost: model.cost.clone(),
            prompt_cache: None,
            context_window: model.context_window,
            max_tokens: model.max_tokens,
            headers: None,
            compat: None,
            sampling_params: None,
            sampling_params_by_thinking_level: None,
        })
        .collect()
}

/// `getRadiusModels` — models from the credential's cached gateway config;
/// empty without one (the dynamic overlay only — the static public catalog
/// baseline lives in `providers::radius`, 4d38031fb).
pub fn get_radius_models(provider_id: &str, credential: Option<&OAuthCredential>) -> Vec<Model> {
    match get_radius_credential_config(credential) {
        Some(config) => get_radius_models_from_config(provider_id, &config),
        None => Vec::new(),
    }
}

/// `truncateHttpBody`.
fn truncate_http_body(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.chars().count() > 512 {
        let truncated: String = trimmed.chars().take(512).collect();
        format!("{truncated}…")
    } else {
        trimmed.to_owned()
    }
}

/// `loadRadiusGatewayConfig` — `GET {gateway}/v1/config`, optional Bearer
/// auth, sanitized JSON body.
pub async fn load_radius_gateway_config(
    gateway: &str,
    api_key: Option<&str>,
    signal: Option<&CancellationToken>,
) -> Result<RadiusGatewayConfig, ModelsError> {
    let config_url = format!("{gateway}/v1/config");
    let mut request = crate::api::http_client::adapter_client_builder(None, &config_url)
        .and_then(|builder| builder.build().map_err(|error| error.to_string()))
        .map_err(|error| ModelsError::new(ModelsErrorCode::ModelSource, error))?
        .get(&config_url)
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(api_key) = api_key {
        request = request.header(reqwest::header::AUTHORIZATION, format!("Bearer {api_key}"));
    }
    let send = request.send();
    let response = match signal {
        Some(token) => {
            tokio::select! {
                () = token.cancelled() => {
                    return Err(ModelsError::new(
                        ModelsErrorCode::ModelSource,
                        format!("Could not load Radius config from {gateway}: aborted"),
                    ));
                }
                response = send => response,
            }
        }
        None => send.await,
    };
    let response = response.map_err(|error| {
        ModelsError::with_cause(
            ModelsErrorCode::ModelSource,
            format!("Could not load Radius config from {gateway}"),
            &error.to_string(),
        )
    })?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(ModelsError::new(
            ModelsErrorCode::ModelSource,
            format!(
                "Could not load Radius config from {gateway}: {status}: {}",
                truncate_http_body(&body)
            ),
        ));
    }
    let body: Value = response.json().await.map_err(|error| {
        ModelsError::with_cause(
            ModelsErrorCode::ModelSource,
            format!("Invalid Radius config from {gateway}"),
            &error.to_string(),
        )
    })?;
    sanitize_radius_gateway_config(&body).ok_or_else(|| {
        ModelsError::new(
            ModelsErrorCode::ModelSource,
            format!("Invalid Radius config from {gateway}"),
        )
    })
}

#[cfg(test)]
mod tests {
    //! No dedicated upstream test file for `radius-config.ts`; these pin the
    //! guard/normalization semantics used by `radius.ts` and the W5 OAuth
    //! flow (`test/radius-oauth.test.ts` exercises them end to end there).

    use serde_json::json;

    use super::*;
    use crate::auth::OAuthCredential;

    fn config_json() -> Value {
        json!({
            "baseUrl": "https://radius.pi.dev/api",
            "models": [
                {
                    "id": "radius-large",
                    "name": "Radius Large",
                    "reasoning": true,
                    "input": ["text", "image"],
                    "cost": {"input": 1.0, "output": 2.0, "cacheRead": 0.1, "cacheWrite": 0.2},
                    "contextWindow": 200000,
                    "maxTokens": 8192
                },
                // Invalid entry: filtered out (upstream `filter`).
                {"id": 42, "name": "broken"}
            ]
        })
    }

    #[test]
    fn normalize_adds_scheme_and_strips_trailing_slashes() {
        assert_eq!(
            normalize_radius_gateway_url("radius.pi.dev"),
            "https://radius.pi.dev"
        );
        assert_eq!(
            normalize_radius_gateway_url("http://localhost:8787/"),
            "http://localhost:8787"
        );
        assert_eq!(
            normalize_radius_gateway_url("HTTPS://RADIUS.PI.DEV//"),
            "HTTPS://RADIUS.PI.DEV"
        );
    }

    #[test]
    fn sanitize_filters_invalid_models_and_rejects_bad_configs() {
        let config = sanitize_radius_gateway_config(&config_json()).expect("config");
        assert_eq!(config.base_url, "https://radius.pi.dev/api");
        assert_eq!(config.models.len(), 1);
        assert_eq!(config.models[0].id, "radius-large");

        assert!(sanitize_radius_gateway_config(&json!(null)).is_none());
        assert!(sanitize_radius_gateway_config(&json!([])).is_none());
        assert!(sanitize_radius_gateway_config(&json!({"models": []})).is_none());
        assert!(sanitize_radius_gateway_config(&json!({"baseUrl": 1, "models": []})).is_none());
    }

    #[test]
    fn materializes_models_onto_pi_messages() {
        let config = sanitize_radius_gateway_config(&config_json()).expect("config");
        let models = get_radius_models_from_config("radius", &config);
        assert_eq!(models.len(), 1);
        let model = &models[0];
        assert_eq!(model.api.as_str(), ApiKind::RPI_MESSAGES);
        assert_eq!(model.provider, "radius");
        assert_eq!(model.base_url, "https://radius.pi.dev/api");
        assert_eq!(model.input, vec![InputModality::Text, InputModality::Image]);
        assert!(model.reasoning);
    }

    #[test]
    fn credential_config_reads_the_gateway_config_extra() {
        let mut extra = serde_json::Map::new();
        extra.insert("gatewayConfig".to_owned(), config_json());
        let credential = OAuthCredential {
            refresh: "r".to_owned(),
            access: "a".to_owned(),
            expires: i64::MAX,
            extra,
        };
        let models = get_radius_models("radius", Some(&credential));
        assert_eq!(models.len(), 1);

        // Malformed gatewayConfig behaves like a missing one.
        let mut extra = serde_json::Map::new();
        extra.insert("gatewayConfig".to_owned(), json!({"baseUrl": 1}));
        let credential = OAuthCredential {
            extra,
            ..credential
        };
        assert!(get_radius_models("radius", Some(&credential)).is_empty());
        assert!(get_radius_models("radius", None).is_empty());
    }

    #[test]
    fn truncate_http_body_caps_at_512_chars() {
        let long = "x".repeat(600);
        let truncated = truncate_http_body(&long);
        assert_eq!(truncated.chars().count(), 513);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncate_http_body("  short  "), "short");
    }
}
