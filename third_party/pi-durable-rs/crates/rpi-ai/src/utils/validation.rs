//! Port of `packages/ai/src/utils/validation.ts` @ pi 0.84.1+ (4181f66).
//!
//! Tool-call argument validation against JSON Schema with lenient type
//! coercion (null→0, "123"→123, …), recursion through allOf/anyOf/oneOf,
//! type arrays, object properties/additionalProperties and array items
//! (tuple positional).
//!
//! Intentional differences:
//! - Upstream distinguishes TypeBox schemas (symbol-marked, `Value.Convert`
//!   prelude) from plain JSON Schema. In rpi every tool parameter schema is
//!   plain JSON, so all schemas take the recursive-coercion path and the
//!   TypeBox `Value.Convert` prelude has no equivalent.
//! - Validation wording comes from the `jsonschema` crate, not TypeBox; the
//!   surrounding message format (`Validation failed for tool "…"` +
//!   `- path: message` lines + `Received arguments:` pretty JSON) is ported
//!   exactly. Registered as deviation D-006.

use serde_json::{Map, Value};

use crate::types::{Tool, ToolCall};

fn get_schema_types(schema: &Value) -> Vec<&str> {
    match schema.get("type") {
        Some(Value::String(kind)) => vec![kind.as_str()],
        Some(Value::Array(kinds)) => kinds.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    }
}

fn is_integer_value(value: &Value) -> bool {
    match value {
        Value::Number(number) => {
            number.is_i64()
                || number.is_u64()
                || number.as_f64().map(|f| f.fract() == 0.0).unwrap_or(false)
        }
        _ => false,
    }
}

fn matches_json_type(value: &Value, kind: &str) -> bool {
    match kind {
        "number" => value.is_number(),
        "integer" => is_integer_value(value),
        "boolean" => value.is_boolean(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        _ => false,
    }
}

fn get_sub_schema_validator(schema: &Value) -> Option<jsonschema::Validator> {
    // Draft 7 matches TypeBox semantics (tuple `items` arrays, etc.); the
    // crate default (2020-12) rejects tuple `items` schemas that TypeBox
    // accepts.
    jsonschema::draft7::new(schema).ok()
}

/// JS `String(number)`: integral floats render without a trailing `.0`.
fn js_number_to_string(number: &serde_json::Number) -> String {
    if let Some(f) = number.as_f64()
        && f.fract() == 0.0
        && f.is_finite()
        && f.abs() < 1e15
    {
        return format!("{f:.0}");
    }
    number.to_string()
}

/// Coerced numbers that are integral become integer JSON numbers (JS numbers
/// are all doubles but serialize integral values without a fraction).
fn f64_to_value(parsed: f64) -> Value {
    if parsed.fract() == 0.0 && parsed.is_finite() && parsed.abs() < 9.0e15 {
        return Value::from(parsed as i64);
    }
    serde_json::Number::from_f64(parsed)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn coerce_primitive_by_type(value: &Value, kind: &str) -> Value {
    match kind {
        "number" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Value::String(text) = value
                && !text.trim().is_empty()
                && let Ok(parsed) = text.trim().parse::<f64>()
                && parsed.is_finite()
            {
                return f64_to_value(parsed);
            }
            if let Value::Bool(b) = value {
                return Value::from(if *b { 1 } else { 0 });
            }
            value.clone()
        }
        "integer" => {
            if value.is_null() {
                return Value::from(0);
            }
            if let Value::String(text) = value
                && !text.trim().is_empty()
                && let Ok(parsed) = text.trim().parse::<f64>()
                && parsed.fract() == 0.0
                && parsed.is_finite()
            {
                return f64_to_value(parsed);
            }
            if let Value::Bool(b) = value {
                return Value::from(if *b { 1 } else { 0 });
            }
            value.clone()
        }
        "boolean" => {
            if value.is_null() {
                return Value::from(false);
            }
            if let Value::String(text) = value {
                if text == "true" {
                    return Value::from(true);
                }
                if text == "false" {
                    return Value::from(false);
                }
            }
            if let Some(number) = value.as_f64() {
                if number == 1.0 {
                    return Value::from(true);
                }
                if number == 0.0 {
                    return Value::from(false);
                }
            }
            value.clone()
        }
        "string" => {
            if value.is_null() {
                return Value::from(String::new());
            }
            match value {
                Value::Number(number) => Value::from(js_number_to_string(number)),
                Value::Bool(b) => Value::from(b.to_string()),
                _ => value.clone(),
            }
        }
        "null" => match value {
            Value::String(text) if text.is_empty() => Value::Null,
            Value::Number(number) if number.as_f64() == Some(0.0) => Value::Null,
            Value::Bool(false) => Value::Null,
            _ => value.clone(),
        },
        _ => value.clone(),
    }
}

fn apply_schema_object_coercion(value: &mut Map<String, Value>, schema: &Value) {
    let properties = schema.get("properties").and_then(Value::as_object);
    if let Some(properties) = properties {
        for (key, property_schema) in properties {
            if let Some(entry) = value.get_mut(key) {
                let coerced = coerce_with_json_schema(entry.clone(), property_schema);
                *entry = coerced;
            }
        }
    }

    if let Some(additional) = schema.get("additionalProperties")
        && additional.is_object()
    {
        let defined_keys: Vec<String> = properties
            .map(|p| p.keys().cloned().collect())
            .unwrap_or_default();
        let keys: Vec<String> = value.keys().cloned().collect();
        for key in keys {
            if defined_keys.contains(&key) {
                continue;
            }
            if let Some(entry) = value.get_mut(&key) {
                let coerced = coerce_with_json_schema(entry.clone(), additional);
                *entry = coerced;
            }
        }
    }
}

fn apply_schema_array_coercion(value: &mut [Value], schema: &Value) {
    match schema.get("items") {
        Some(Value::Array(item_schemas)) => {
            for (index, item) in value.iter_mut().enumerate() {
                let Some(item_schema) = item_schemas.get(index) else {
                    continue;
                };
                let coerced = coerce_with_json_schema(item.clone(), item_schema);
                *item = coerced;
            }
        }
        Some(items @ Value::Object(_)) => {
            for item in value.iter_mut() {
                let coerced = coerce_with_json_schema(item.clone(), items);
                *item = coerced;
            }
        }
        _ => {}
    }
}

fn coerce_with_union_schema(value: &Value, schemas: &[Value]) -> Value {
    // 2e95584da: a value that already matches a union arm is preserved as-is
    // before any coercion is attempted (nullable unions must not convert
    // `null` into another primitive).
    for schema in schemas {
        if let Some(validator) = get_sub_schema_validator(schema)
            && validator.is_valid(value)
        {
            return value.clone();
        }
    }

    for schema in schemas {
        let candidate = value.clone();
        let coerced = coerce_with_json_schema(candidate, schema);
        if let Some(validator) = get_sub_schema_validator(schema)
            && validator.is_valid(&coerced)
        {
            return coerced;
        }
    }
    value.clone()
}

fn coerce_with_json_schema(value: Value, schema: &Value) -> Value {
    let mut next_value = value;

    if let Some(Value::Array(all_of)) = schema.get("allOf") {
        for nested in all_of {
            next_value = coerce_with_json_schema(next_value, nested);
        }
    }

    if let Some(Value::Array(any_of)) = schema.get("anyOf") {
        next_value = coerce_with_union_schema(&next_value, any_of);
    }

    if let Some(Value::Array(one_of)) = schema.get("oneOf") {
        next_value = coerce_with_union_schema(&next_value, one_of);
    }

    let schema_types = get_schema_types(schema);
    let matches_union_member = schema_types.len() > 1
        && schema_types
            .iter()
            .any(|kind| matches_json_type(&next_value, kind));
    if !schema_types.is_empty() && !matches_union_member {
        for kind in &schema_types {
            let candidate = coerce_primitive_by_type(&next_value, kind);
            if candidate != next_value {
                next_value = candidate;
                break;
            }
        }
    }

    if schema_types.contains(&"object")
        && let Value::Object(map) = &mut next_value
    {
        apply_schema_object_coercion(map, schema);
    }

    if schema_types.contains(&"array")
        && let Value::Array(items) = &mut next_value
    {
        apply_schema_array_coercion(items, schema);
    }

    next_value
}

fn get_validator(schema: &Value) -> Result<jsonschema::Validator, String> {
    jsonschema::draft7::new(schema).map_err(|error| error.to_string())
}

fn format_validation_path(error: &jsonschema::ValidationError) -> String {
    let instance_path = error.instance_path.to_string();
    let base_path = instance_path
        .strip_prefix('/')
        .unwrap_or(&instance_path)
        .replace('/', ".");
    if let jsonschema::error::ValidationErrorKind::Required { property } = &error.kind {
        // `property` is a JSON value like `"/name"` (pointer-escaped).
        let property = property
            .as_str()
            .map(|p| p.strip_prefix('/').unwrap_or(p).to_owned())
            .unwrap_or_default();
        if !property.is_empty() {
            return if base_path.is_empty() {
                property
            } else {
                format!("{base_path}.{property}")
            };
        }
    }
    if base_path.is_empty() {
        "root".to_owned()
    } else {
        base_path
    }
}

/// `validateToolCall`: finds a tool by name and validates the call arguments.
pub fn validate_tool_call(tools: &[Tool], tool_call: &ToolCall) -> Result<Value, String> {
    let Some(tool) = tools.iter().find(|t| t.name == tool_call.name) else {
        return Err(format!("Tool \"{}\" not found", tool_call.name));
    };
    validate_tool_arguments(tool, tool_call)
}

/// `validateToolArguments`: validates (and coerces) tool-call arguments
/// against the tool's JSON schema. Returns the validated arguments.
/// `getSubSchemaValidator(propertySchema)?.Check(null)` — does the property
/// schema accept `null`? A schema that fails to compile keeps the key
/// (upstream `undefined?.Check(null) === false` → false).
fn schema_accepts_null(schema: &Value) -> bool {
    jsonschema::draft7::new(schema)
        .map(|validator| validator.is_valid(&Value::Null))
        .unwrap_or(true)
}

/// `normalizeOptionalNulls` (validation.ts:240-268, `7915cdac6`): drop
/// `null` values for optional, non-nullable, non-`$ref` properties
/// (recursively through arrays and nested objects) before coercion —
/// providers reject explicit nulls on optional fields.
fn normalize_optional_nulls(value: &mut Value, schema: &Value) {
    let Some(schema_object) = schema.as_object() else {
        return;
    };
    if let Value::Array(items) = value {
        match schema_object.get("items") {
            Some(Value::Array(item_schemas)) => {
                for (item, item_schema) in items.iter_mut().zip(item_schemas.iter()) {
                    normalize_optional_nulls(item, item_schema);
                }
            }
            Some(single) => {
                for item in items.iter_mut() {
                    normalize_optional_nulls(item, single);
                }
            }
            None => {}
        }
        return;
    }
    let Some(object) = value.as_object_mut() else {
        return;
    };
    let Some(properties) = schema_object.get("properties").and_then(Value::as_object) else {
        return;
    };
    let required: std::collections::HashSet<&str> = schema_object
        .get("required")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::HashSet<_>>()
        })
        .unwrap_or_default();
    for (key, property_schema) in properties {
        if !object.contains_key(key) {
            continue;
        }
        let is_null = object.get(key) == Some(&Value::Null);
        let ref_exempt = property_schema.get("$ref").is_some_and(Value::is_string);
        if is_null
            && !required.contains(key.as_str())
            && !ref_exempt
            && !schema_accepts_null(property_schema)
        {
            object.remove(key);
        } else {
            let Some(child) = object.get_mut(key) else {
                continue;
            };
            normalize_optional_nulls(child, property_schema);
        }
    }
}

pub fn validate_tool_arguments(tool: &Tool, tool_call: &ToolCall) -> Result<Value, String> {
    let mut args = Value::Object(tool_call.arguments.clone());
    normalize_optional_nulls(&mut args, &tool.parameters);
    let validator = get_validator(&tool.parameters)?;

    let coerced = coerce_with_json_schema(args.clone(), &tool.parameters);
    if coerced != args {
        if args.is_object() && coerced.is_object() {
            args = coerced;
        } else {
            return Ok(if validator.is_valid(&coerced) {
                coerced
            } else {
                args
            });
        }
    }

    if validator.is_valid(&args) {
        return Ok(args);
    }

    let errors = validator
        .iter_errors(&args)
        .map(|error| format!("  - {}: {}", format_validation_path(&error), error))
        .collect::<Vec<_>>()
        .join("\n");
    let errors = if errors.is_empty() {
        "Unknown validation error".to_owned()
    } else {
        errors
    };

    let received = serde_json::to_string_pretty(&tool_call.arguments)
        .unwrap_or_else(|_| "[unserializable]".to_owned());
    Err(format!(
        "Validation failed for tool \"{}\":\n{errors}\n\nReceived arguments:\n{received}",
        tool_call.name
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(parameters: Value) -> Tool {
        Tool {
            name: "bash".to_owned(),
            description: "run".to_owned(),
            parameters,
            constrained_sampling: None,
        }
    }

    fn call(arguments: Value) -> ToolCall {
        ToolCall {
            id: "c1".to_owned(),
            name: "bash".to_owned(),
            arguments: arguments.as_object().cloned().unwrap_or_default(),
            thought_signature: None,
            namespace: None,
        }
    }

    /// "treats null as omission for optional non-nullable properties"
    /// (validation.test.ts:101-124, `7915cdac6`): optional non-nullable
    /// nulls drop (recursively); nullable-union nulls survive.
    #[test]
    fn test_normalize_optional_nulls_drops_non_nullable_optionals() {
        let tool = tool(json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "number"},
                "nullable": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                "metadata": {
                    "type": "object",
                    "properties": {"enabled": {"type": "boolean"}},
                    "required": []
                }
            },
            "required": ["path"]
        }));
        let out = validate_tool_arguments(
            &tool,
            &call(json!({
                "path": "file.txt",
                "offset": null,
                "nullable": null,
                "metadata": {"enabled": null}
            })),
        )
        .expect("valid");
        assert_eq!(
            out,
            json!({"path": "file.txt", "nullable": null, "metadata": {}})
        );
    }

    /// "preserves optional nulls whose referenced schema is nullable"
    /// (validation.test.ts:126-143): `$ref` properties are exempt from the
    /// null cleanup.
    #[test]
    fn test_normalize_optional_nulls_exempts_refs() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"value": {"$ref": "#/$defs/value"}},
            "$defs": {"value": {"anyOf": [{"type": "number"}, {"type": "null"}]}}
        }));
        let out = validate_tool_arguments(&tool, &call(json!({"value": null}))).expect("valid");
        assert_eq!(out, json!({"value": null}));
    }

    /// Array items recurse through both tuple and single `items` schemas.
    #[test]
    fn test_normalize_optional_nulls_recurses_arrays() {
        let tool = tool(json!({
            "type": "object",
            "properties": {
                "tags": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {"label": {"type": "string"}},
                        "required": []
                    }
                }
            },
            "required": []
        }));
        let out = validate_tool_arguments(
            &tool,
            &call(json!({"tags": [{"label": null}, {"label": "keep"}]})),
        )
        .expect("valid");
        assert_eq!(out, json!({"tags": [{}, {"label": "keep"}]}));
    }

    #[test]
    fn test_validate_tool_call_not_found() {
        let err = validate_tool_call(&[], &call(json!({}))).unwrap_err();
        assert_eq!(err, "Tool \"bash\" not found");
    }

    #[test]
    fn test_coercion_null_to_number() {
        // `required` mirrors the upstream coercion harness
        // (createToolCallWithPlainSchema): required properties skip the
        // optional-null cleanup and reach coercion (7915cdac6 changed
        // optional nulls to deletion — see the normalize tests).
        let tool = tool(json!({
            "type": "object",
            "properties": {"n": {"type": "number"}, "i": {"type": "integer"}},
            "required": ["n", "i"]
        }));
        let out =
            validate_tool_arguments(&tool, &call(json!({"n": null, "i": null}))).expect("valid");
        assert_eq!(out, json!({"n": 0, "i": 0}));
    }

    #[test]
    fn test_coercion_string_to_number_and_integer() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"n": {"type": "number"}, "i": {"type": "integer"}}
        }));
        let out =
            validate_tool_arguments(&tool, &call(json!({"n": "123", "i": "42"}))).expect("valid");
        assert_eq!(out, json!({"n": 123, "i": 42}));
        // Non-integer string into integer field: not coerced, fails validation.
        let result = validate_tool_arguments(&tool, &call(json!({"n": 1, "i": "4.5"})));
        assert!(result.is_err());
        // Empty string is not coerced (JS Number("") guard).
        let result = validate_tool_arguments(&tool, &call(json!({"n": "", "i": 1})));
        assert!(result.is_err());
    }

    #[test]
    fn test_coercion_bool_to_number_and_back() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"n": {"type": "number"}, "b": {"type": "boolean"}}
        }));
        let out = validate_tool_arguments(&tool, &call(json!({"n": true, "b": 1}))).expect("valid");
        assert_eq!(out, json!({"n": 1, "b": true}));
        let out = validate_tool_arguments(&tool, &call(json!({"n": false, "b": "false"})))
            .expect("valid");
        assert_eq!(out, json!({"n": 0, "b": false}));
    }

    #[test]
    fn test_coercion_to_string_and_null() {
        let tool = tool(json!({
            "type": "object",
            "properties": {
                "s": {"type": "string"},
                "x": {"type": "null"}
            },
            "required": ["s", "x"]
        }));
        let out =
            validate_tool_arguments(&tool, &call(json!({"s": 12, "x": false}))).expect("valid");
        assert_eq!(out, json!({"s": "12", "x": null}));
        let out = validate_tool_arguments(&tool, &call(json!({"s": null, "x": 0}))).expect("valid");
        assert_eq!(out, json!({"s": "", "x": null}));
    }

    #[test]
    fn test_coercion_nested_objects_and_arrays() {
        let tool = tool(json!({
            "type": "object",
            "properties": {
                "opts": {
                    "type": "object",
                    "properties": {"timeout": {"type": "integer"}},
                    "additionalProperties": {"type": "string"}
                },
                "list": {"type": "array", "items": {"type": "integer"}},
                "tuple": {"type": "array", "items": [{"type": "integer"}, {"type": "boolean"}]}
            }
        }));
        let out = validate_tool_arguments(
            &tool,
            &call(json!({
                "opts": {"timeout": "30", "extra": 5},
                "list": ["1", 2, "3"],
                "tuple": ["7", "true"]
            })),
        )
        .expect("valid");
        assert_eq!(
            out,
            json!({
                "opts": {"timeout": 30, "extra": "5"},
                "list": [1, 2, 3],
                "tuple": [7, true]
            })
        );
    }

    #[test]
    fn test_coercion_any_of_first_valid_branch() {
        let tool = tool(json!({
            "type": "object",
            "properties": {
                "v": {"anyOf": [{"type": "integer"}, {"type": "string"}]}
            }
        }));
        // 2e95584da: "42" already matches the `string` arm, so it is preserved
        // as-is (pre-fix this was coerced to the integer 42).
        let out = validate_tool_arguments(&tool, &call(json!({"v": "42"}))).expect("valid");
        assert_eq!(out, json!({"v": "42"}));
        let out = validate_tool_arguments(&tool, &call(json!({"v": "abc"}))).expect("valid");
        assert_eq!(out, json!({"v": "abc"}));
    }

    #[test]
    fn test_coercion_type_array_first_convertible() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"v": {"type": ["integer", "string"]}},
            "required": ["v"]
        }));
        // Already matches a union member: left unchanged.
        let out = validate_tool_arguments(&tool, &call(json!({"v": "keep"}))).expect("valid");
        assert_eq!(out, json!({"v": "keep"}));
        // Matches none: first convertible type wins.
        let out = validate_tool_arguments(&tool, &call(json!({"v": null}))).expect("valid");
        assert_eq!(out, json!({"v": 0}));
    }

    // -- union arm preservation (2e95584da / f9476a61e @ 4181f66) -------------

    /// validation.test.ts: "preserves a value that already matches a nullable
    /// union arm".
    #[test]
    fn preserves_a_value_that_already_matches_a_nullable_union_arm() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"value": {"anyOf": [{"type": "number"}, {"type": "null"}]}}
        }));
        let out = validate_tool_arguments(&tool, &call(json!({"value": null}))).expect("valid");
        assert_eq!(out, json!({"value": null}));
    }

    /// validation.test.ts: "preserves a value that already matches a oneOf
    /// nullable union arm".
    #[test]
    fn preserves_a_value_that_already_matches_a_one_of_nullable_union_arm() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"value": {"oneOf": [{"type": "number"}, {"type": "null"}]}}
        }));
        let out = validate_tool_arguments(&tool, &call(json!({"value": null}))).expect("valid");
        assert_eq!(out, json!({"value": null}));
    }

    /// validation.test.ts: "still coerces nullable unions when the original
    /// value does not match any arm".
    #[test]
    fn still_coerces_nullable_unions_when_the_original_value_does_not_match_any_arm() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"value": {"anyOf": [{"type": "number"}, {"type": "null"}]}}
        }));
        let out = validate_tool_arguments(&tool, &call(json!({"value": "42"}))).expect("valid");
        assert_eq!(out, json!({"value": 42}));
    }

    /// validation.test.ts: "accepts null for nullable array schemas with
    /// items" (f9476a61e; the TypeBox `Compile` assertion is TypeBox-specific —
    /// rpi's jsonschema draft-7 validator is the only engine, so the intent
    /// port asserts `validateToolArguments` accepts `null` unchanged).
    #[test]
    fn accepts_null_for_nullable_array_schemas_with_items() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"value": {"type": ["array", "null"], "items": {"type": "string"}}}
        }));
        let out = validate_tool_arguments(&tool, &call(json!({"value": null}))).expect("valid");
        assert_eq!(out, json!({"value": null}));
    }

    #[test]
    fn test_validation_failure_message_format() {
        let tool = tool(json!({
            "type": "object",
            "properties": {"cmd": {"type": "string"}},
            "required": ["cmd"]
        }));
        let err = validate_tool_arguments(&tool, &call(json!({}))).unwrap_err();
        assert!(
            err.starts_with("Validation failed for tool \"bash\":\n"),
            "{err}"
        );
        assert!(err.contains("  - cmd: "), "{err}");
        assert!(err.contains("\n\nReceived arguments:\n{}"), "{err}");
    }
}
