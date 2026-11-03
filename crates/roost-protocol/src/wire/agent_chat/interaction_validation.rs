//! Validates browser responses against the worker's pending ACP request.
//! The same schema normalization guards coordinator admission and worker response.

use super::OmpInteraction;
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OmpInteractionValidationError {
    #[error("the ACP interaction is no longer pending")]
    StaleRequest,
    #[error("the interaction action is not valid for this request")]
    InvalidAction,
    #[error("the response does not satisfy the ACP form schema")]
    InvalidContent,
}

/// Validates an explicit OMP decision and returns normalized content, including schema defaults.
/// Cancellation and decline return JSON null; invalid or stale decisions return an error.
pub fn validate_omp_interaction_response(
    interaction: &OmpInteraction,
    request_id: &str,
    action: &str,
    content_json: &str,
) -> Result<Value, OmpInteractionValidationError> {
    if request_id != interaction.id {
        return Err(OmpInteractionValidationError::StaleRequest);
    }
    let Some(kind) = interaction.request.get("kind").and_then(Value::as_str) else {
        return Err(OmpInteractionValidationError::InvalidAction);
    };
    match (kind, action) {
        ("permission", "cancel") | ("form", "decline" | "cancel") => Ok(Value::Null),
        ("permission", "permission") => {
            let content = parse_content(content_json)?;
            let Some(option_id) = content.get("optionId").and_then(Value::as_str) else {
                return Err(OmpInteractionValidationError::InvalidContent);
            };
            let offered = interaction.request["options"]
                .as_array()
                .is_some_and(|options| {
                    options.iter().any(|option| option["optionId"] == option_id)
                });
            if offered {
                Ok(content)
            } else {
                Err(OmpInteractionValidationError::InvalidContent)
            }
        }
        ("form", "accept") => {
            let mut content = parse_content(content_json)?;
            if normalize_form_content(&interaction.request, &mut content) {
                Ok(content)
            } else {
                Err(OmpInteractionValidationError::InvalidContent)
            }
        }
        _ => Err(OmpInteractionValidationError::InvalidAction),
    }
}

fn parse_content(content_json: &str) -> Result<Value, OmpInteractionValidationError> {
    serde_json::from_str(content_json).map_err(|_| OmpInteractionValidationError::InvalidContent)
}

fn normalize_form_content(request: &Value, content: &mut Value) -> bool {
    let Some(schema) = request.get("requestedSchema") else {
        return false;
    };
    let (Some(properties), Some(values)) =
        (schema["properties"].as_object(), content.as_object_mut())
    else {
        return false;
    };
    if values.keys().any(|key| !properties.contains_key(key)) {
        return false;
    }
    let required = schema["required"].as_array();
    if required.is_some_and(|required| {
        required.iter().any(|name| {
            name.as_str()
                .is_none_or(|name| !properties.contains_key(name))
        })
    }) {
        return false;
    }
    let mut normalized = Map::new();
    for (key, property) in properties {
        let value = values.get(key).or_else(|| property.get("default"));
        let Some(value) = value else {
            if required
                .is_some_and(|required| required.iter().any(|name| name.as_str() == Some(key)))
            {
                return false;
            }
            continue;
        };
        if !validate_property(property, value) {
            return false;
        }
        normalized.insert(key.clone(), value.clone());
    }
    *values = normalized;
    true
}

fn validate_property(schema: &Value, value: &Value) -> bool {
    let valid_type = match schema["type"].as_str() {
        Some("string") => value.as_str().is_some_and(|text| {
            let length = text.chars().count() as u64;
            let pattern_matches = schema["pattern"].as_str().is_none_or(|pattern| {
                regex::Regex::new(pattern).is_ok_and(|pattern| pattern.is_match(text))
            });
            length >= schema["minLength"].as_u64().unwrap_or(0)
                && schema["maxLength"].as_u64().is_none_or(|max| length <= max)
                && pattern_matches
        }),
        Some("boolean") => value.as_bool().is_some(),
        Some("number") => value.as_f64().is_some_and(|number| {
            schema["minimum"].as_f64().is_none_or(|min| number >= min)
                && schema["maximum"].as_f64().is_none_or(|max| number <= max)
        }),
        Some("integer") => integer_value(value).is_some_and(|number| {
            integer_value(&schema["minimum"]).is_none_or(|minimum| number >= minimum)
                && integer_value(&schema["maximum"]).is_none_or(|maximum| number <= maximum)
        }),
        Some("array") => value.as_array().is_some_and(|items| {
            let allowed: Option<Vec<&Value>> = schema
                .pointer("/items/enum")
                .and_then(Value::as_array)
                .map(|values| values.iter().collect())
                .or_else(|| {
                    schema
                        .pointer("/items/anyOf")
                        .and_then(Value::as_array)
                        .map(|options| {
                            options
                                .iter()
                                .flat_map(|option| {
                                    option
                                        .get("const")
                                        .or_else(|| option.get("value"))
                                        .into_iter()
                                        .chain(
                                            option
                                                .get("enum")
                                                .and_then(Value::as_array)
                                                .into_iter()
                                                .flatten(),
                                        )
                                })
                                .collect()
                        })
                });
            let Some(allowed) = allowed else {
                return false;
            };
            let unique_valid = !schema["uniqueItems"].as_bool().unwrap_or(false)
                || items.iter().collect::<std::collections::HashSet<_>>().len() == items.len();
            items
                .iter()
                .all(|item| item.as_str().is_some() && allowed.contains(&item))
                && unique_valid
                && schema["minItems"]
                    .as_u64()
                    .is_none_or(|min| items.len() as u64 >= min)
                && schema["maxItems"]
                    .as_u64()
                    .is_none_or(|max| items.len() as u64 <= max)
        }),
        _ => false,
    };
    valid_type
        && schema
            .get("enum")
            .and_then(Value::as_array)
            .is_none_or(|choices| choices.contains(value))
        && schema
            .get("oneOf")
            .and_then(Value::as_array)
            .is_none_or(|choices| {
                choices
                    .iter()
                    .filter(|choice| choice_matches(choice, value))
                    .count()
                    == 1
            })
}

fn integer_value(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

fn choice_matches(choice: &Value, value: &Value) -> bool {
    choice.get("const").or_else(|| choice.get("value")) == Some(value)
        || choice
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|choices| choices.contains(value))
}

#[cfg(test)]
mod tests {
    use super::{OmpInteractionValidationError as Error, validate_omp_interaction_response};
    use crate::wire::agent_chat::OmpInteraction;
    use serde_json::{Value, json};

    fn interaction(request: Value) -> OmpInteraction {
        OmpInteraction {
            id: "run:2:17".into(),
            request,
        }
    }

    #[test]
    fn form_validation_normalizes_defaults_and_enforces_types_ranges_and_choices() {
        let interaction = interaction(json!({
            "kind":"form",
            "mode":"form",
            "requestedSchema":{
                "type":"object","required":["category"],
                "properties":{
                    "category":{"type":"string","oneOf":[{"const":"fast"},{"const":"safe"}]},
                    "size":{"type":"string","enum":["small","large"],"default":"small"},
                    "enabled":{"type":"boolean","default":false},
                    "count":{"type":"integer","minimum":1,"maximum":4,"default":2},
                    "score":{"type":"number","minimum":0.5,"maximum":2.0,"default":1.25},
                    "labels":{"type":"array","items":{"anyOf":[{"const":"a"},{"const":"b"}]},
                        "minItems":1,"maxItems":2,"default":["a"]}
                }
            }
        }));
        let normalized = validate_omp_interaction_response(
            &interaction,
            "run:2:17",
            "accept",
            r#"{"category":"fast"}"#,
        )
        .ok();
        assert_eq!(
            normalized,
            Some(json!({
                "category":"fast","size":"small","enabled":false,"count":2,
                "score":1.25,"labels":["a"]
            }))
        );
        assert_eq!(
            validate_omp_interaction_response(&interaction, "run:2:17", "accept", "{}"),
            Err(Error::InvalidContent),
        );
        assert_eq!(
            validate_omp_interaction_response(
                &interaction,
                "run:2:17",
                "accept",
                r#"{"category":"fast","enabled":"false"}"#
            ),
            Err(Error::InvalidContent),
        );
    }

    #[test]
    fn arrays_enforce_minimum_unique_values_and_offered_options() {
        let interaction = interaction(json!({
            "kind":"form","mode":"form","requestedSchema":{"type":"object","properties":{
                "choices":{"type":"array","items":{"enum":["a","b"]},"minItems":1,"uniqueItems":true}
            }}
        }));
        assert_eq!(
            validate_omp_interaction_response(
                &interaction,
                "run:2:17",
                "accept",
                r#"{"choices":[]}"#
            ),
            Err(Error::InvalidContent),
        );
        assert_eq!(
            validate_omp_interaction_response(
                &interaction,
                "run:2:17",
                "accept",
                r#"{"choices":["a","a"]}"#
            ),
            Err(Error::InvalidContent),
        );
        assert_eq!(
            validate_omp_interaction_response(
                &interaction,
                "run:1:17",
                "accept",
                r#"{"choices":["a"]}"#
            ),
            Err(Error::StaleRequest),
        );
    }

    #[test]
    fn permission_responses_require_an_offered_option_and_explicit_cancel() {
        let interaction = interaction(json!({
            "kind":"permission","options":[{"optionId":"allow-once"}]
        }));
        assert_eq!(
            validate_omp_interaction_response(
                &interaction,
                "run:2:17",
                "permission",
                r#"{"optionId":"allow-always"}"#
            ),
            Err(Error::InvalidContent),
        );
        assert_eq!(
            validate_omp_interaction_response(
                &interaction,
                "run:2:17",
                "permission",
                r#"{"optionId":"allow-once"}"#
            )
            .ok(),
            Some(json!({"optionId":"allow-once"})),
        );
        assert_eq!(
            validate_omp_interaction_response(&interaction, "run:2:17", "cancel", ""),
            Ok(Value::Null),
        );
    }
}
