//! ACP config option projection into Roost's OMP machine catalogs.
//! The writer persists the original options and settings RPCs expose choices.
//! Missing option ids leave OMP's own configuration authoritative.

use roost_protocol::wire::agent_chat::{OmpChoice, OmpWorkerCatalog};
use serde_json::Value;

pub fn catalog_from_config_options(
    worker_fp: &str,
    options: &Value,
    modes: Vec<OmpChoice>,
    now_ms: i64,
) -> OmpWorkerCatalog {
    let choices = |id| {
        option(options, id)
            .and_then(|option| option.get("options"))
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .filter_map(|option| {
                        Some(OmpChoice {
                            value: option.get("value")?.as_str()?.into(),
                            name: option.get("name")?.as_str()?.into(),
                            description: option
                                .get("description")
                                .and_then(Value::as_str)
                                .map(Into::into),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    OmpWorkerCatalog {
        worker_fp: worker_fp.into(),
        models: choices("model"),
        thinking_levels: choices("thinking"),
        modes,
        updated_ms: now_ms,
    }
}

pub fn current_values(options: &Value) -> (Option<String>, Option<String>, Option<String>) {
    let current = |id| {
        option(options, id)
            .and_then(|option| option.get("currentValue"))
            .and_then(Value::as_str)
            .map(Into::into)
    };
    (current("model"), current("thinking"), current("mode"))
}

fn option<'a>(options: &'a Value, id: &str) -> Option<&'a Value> {
    options
        .as_array()?
        .iter()
        .find(|option| option.get("id").and_then(Value::as_str) == Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn catalog_projects_only_live_acp_choices() {
        let options = json!([
            {
                "id": "model",
                "currentValue": "openai-codex/gpt-5",
                "options": [
                    {"value": "openai-codex/gpt-5", "name": "GPT 5", "description": "Connected"},
                    {"value": "anthropic/sonnet", "name": "Sonnet"}
                ]
            },
            {
                "id": "thinking",
                "currentValue": "high",
                "options": [{"value": "high", "name": "High"}]
            },
            {
                "id": "mode",
                "currentValue": "default",
                "options": [{"value": "default", "name": "Default"}]
            }
        ]);

        let modes = vec![OmpChoice {
            value: "plan".into(),
            name: "Plan".into(),
            description: Some("Plan before acting".into()),
        }];
        let catalog = catalog_from_config_options("worker", &options, modes, 42);

        assert_eq!(catalog.worker_fp, "worker");
        assert_eq!(catalog.updated_ms, 42);
        assert_eq!(catalog.modes[0].value, "plan");
        assert_eq!(catalog.modes[0].name, "Plan");
        assert_eq!(catalog.models.len(), 2);
        assert_eq!(catalog.models[0].value, "openai-codex/gpt-5");
        assert_eq!(catalog.models[0].description.as_deref(), Some("Connected"));
        assert_eq!(catalog.thinking_levels.len(), 1);
        assert_eq!(catalog.thinking_levels[0].value, "high");
        assert_eq!(
            current_values(&options),
            (
                Some("openai-codex/gpt-5".into()),
                Some("high".into()),
                Some("default".into())
            )
        );
    }
}
