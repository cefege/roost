//! ACP config option projection into Roost's OMP machine catalogs.
//! The writer persists the original options and settings RPCs expose choices.
//! Missing option ids leave OMP's own configuration authoritative.

use roost_protocol::wire::agent_chat::{OmpChoice, OmpWorkerCatalog};
use serde_json::Value;

pub fn catalog_from_config_options(
    worker_fp: &str,
    options: &Value,
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
