//! Azure provider rename detection (v0.1.7 V17-01, R2.2.2; upstream #9714).
//!
//! Upstream renamed the Azure **provider id** from `azure-openai-responses`
//! to `azure` in v1.0.3 while keeping the `azure-openai-responses` **api id**
//! and the `AZURE_OPENAI_*` environment variables unchanged. Upstream has no
//! detection face: old `auth.json` / `models.json` / `settings.json` keys
//! silently stop matching (the CHANGELOG only carries the migration note).
//! rpi deliberately adds an explicit diagnostic
//! (**rpi-own [VARIANT]**, ADR-0036 decision ④): old keys are reported with
//! the file and key position plus migration guidance, but are **never
//! rewritten, migrated, or aliased** — an old key is not implicitly
//! equivalent to `azure`.
//!
//! This module is the single diagnostic seam used by every loading path:
//! `settings_manager` (settings.json), `models_json` (models.json), and the
//! auth store helpers (auth.json). Legitimate `azure` keys, including
//! `azure/<model>` patterns and `azure/*` globs, never trigger.

use serde_json::{Map, Value};

/// Renamed provider id (upstream `azure-openai-responses`).
pub const LEGACY_AZURE_PROVIDER_ID: &str = "azure-openai-responses";

/// Current provider id (upstream `azure`).
pub const AZURE_PROVIDER_ID: &str = "azure";

/// Shared migration guidance appended to every diagnostic.
pub fn migration_guidance() -> String {
    format!(
        "The Azure provider was renamed from \"{LEGACY_AZURE_PROVIDER_ID}\" to \"{AZURE_PROVIDER_ID}\" in Pi v1.1.0. Update the key to \"{AZURE_PROVIDER_ID}\", or run `/login` again; the azure-openai-responses api id and the AZURE_OPENAI_* environment variables are unchanged."
    )
}

/// Whether a model id / pattern uses the legacy provider:
/// `azure-openai-responses`, `azure-openai-responses/<model>`,
/// `azure-openai-responses/*` (a trailing `:thinking-level` suffix is
/// covered by the prefix form). Plain model ids without the provider segment
/// cannot be attributed and never trigger.
pub fn is_legacy_provider_pattern(value: &str) -> bool {
    value == LEGACY_AZURE_PROVIDER_ID || value.starts_with("azure-openai-responses/")
}

/// One diagnostic for a legacy key occurrence: file + key position +
/// migration guidance.
pub fn legacy_key_diagnostic(file: &str, key_path: &str) -> String {
    format!(
        "Unsupported provider id \"{LEGACY_AZURE_PROVIDER_ID}\" in {file} at {key_path}. {}",
        migration_guidance()
    )
}

/// settings.json detection (R2.2.2 judgment table): the `defaultProvider`
/// provider value, `enabledModels` provider segments / globs, and
/// `modelThinkingLevels` `provider/modelId` keys.
pub fn settings_json_diagnostics(file: &str, settings: &Map<String, Value>) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if settings.get("defaultProvider").and_then(Value::as_str) == Some(LEGACY_AZURE_PROVIDER_ID) {
        diagnostics.push(legacy_key_diagnostic(file, "defaultProvider"));
    }
    if let Some(Value::Array(patterns)) = settings.get("enabledModels") {
        for (index, pattern) in patterns.iter().enumerate() {
            if let Some(pattern) = pattern.as_str()
                && is_legacy_provider_pattern(pattern)
            {
                diagnostics.push(legacy_key_diagnostic(
                    file,
                    &format!("enabledModels[{index}]"),
                ));
            }
        }
    }
    if let Some(Value::Object(levels)) = settings.get("modelThinkingLevels") {
        for key in levels.keys() {
            if is_legacy_provider_pattern(key) {
                diagnostics.push(legacy_key_diagnostic(
                    file,
                    &format!("modelThinkingLevels.{key}"),
                ));
            }
        }
    }
    diagnostics
}

/// models.json detection: a top-level `providers` key still using the legacy
/// provider id. The entry stays inert — registered providers are looked up by
/// id, and no provider answers to the old id (no alias).
pub fn models_json_diagnostics(file: &str, document: &Value) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if let Some(Value::Object(providers)) = document.get("providers") {
        for provider_id in providers.keys() {
            if provider_id == LEGACY_AZURE_PROVIDER_ID {
                diagnostics.push(legacy_key_diagnostic(
                    file,
                    "providers.azure-openai-responses",
                ));
            }
        }
    }
    diagnostics
}

/// auth.json detection: a stored credential still keyed by the legacy
/// provider id. The credential stays inert (the renamed provider reads only
/// the `azure` entry); `/login` writes the new key.
pub fn auth_json_diagnostics(file: &str, data: &Map<String, Value>) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if data.contains_key(LEGACY_AZURE_PROVIDER_ID) {
        diagnostics.push(legacy_key_diagnostic(
            file,
            "azure-openai-responses (credential key)",
        ));
    }
    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn settings_detection_covers_the_judgment_table() {
        let settings = json!({
            "defaultProvider": "azure-openai-responses",
            "enabledModels": [
                "azure-openai-responses/gpt-5.4",
                "azure-openai-responses/*",
                "azure/*",
                "azure/gpt-5.4",
                "gpt-5.4"
            ],
            "modelThinkingLevels": {
                "azure-openai-responses/gpt-5.4": "high",
                "azure/gpt-5.4": "medium"
            }
        });
        let diagnostics =
            settings_json_diagnostics("settings.json", settings.as_object().expect("object"));
        assert_eq!(diagnostics.len(), 4, "{diagnostics:?}");
        assert!(diagnostics[0].contains("defaultProvider"));
        assert!(diagnostics[1].contains("enabledModels[0]"));
        assert!(diagnostics[2].contains("enabledModels[1]"));
        assert!(diagnostics[3].contains("modelThinkingLevels.azure-openai-responses/gpt-5.4"));
        for diagnostic in &diagnostics {
            assert!(diagnostic.contains("settings.json"));
            assert!(diagnostic.contains(&migration_guidance()));
        }
    }

    #[test]
    fn legitimate_azure_keys_never_trigger() {
        let settings = json!({
            "defaultProvider": "azure",
            "enabledModels": ["azure/gpt-5.4", "azure/*", "azure-openai-responses-extra"],
            "modelThinkingLevels": {"azure/gpt-5.4": "high"}
        });
        assert!(
            settings_json_diagnostics("settings.json", settings.as_object().expect("object"))
                .is_empty()
        );
        assert!(
            models_json_diagnostics(
                "models.json",
                &json!({"providers": {"azure": {"baseUrl": "https://example.test"}}})
            )
            .is_empty()
        );
        assert!(
            auth_json_diagnostics(
                "auth.json",
                json!({"azure": {"type": "api_key", "key": "x"}})
                    .as_object()
                    .expect("object")
            )
            .is_empty()
        );
    }

    #[test]
    fn models_and_auth_detection_report_file_and_key() {
        let models = models_json_diagnostics(
            "/home/u/.rpi/agent/models.json",
            &json!({"providers": {"azure-openai-responses": {"baseUrl": "https://x"}}}),
        );
        assert_eq!(models.len(), 1);
        assert!(models[0].contains("providers.azure-openai-responses"));
        assert!(models[0].contains("/home/u/.rpi/agent/models.json"));

        let auth = auth_json_diagnostics(
            "/home/u/.rpi/agent/auth.json",
            json!({"azure-openai-responses": {"type": "api_key", "key": "secret"}})
                .as_object()
                .expect("object"),
        );
        assert_eq!(auth.len(), 1);
        assert!(auth[0].contains("azure-openai-responses (credential key)"));
    }
}
