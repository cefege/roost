//! Port of `packages/ai/src/api/constrained-sampling.ts` @ pi 0.82.1
//! (2efa728).
//!
//! JSON-schema strict sampling (Anthropic) and OpenAI grammar constrained
//! sampling helpers. Fallible operations return `Err(String)` carrying the
//! upstream `Error.message`; adapters surface them through the stream error
//! path.

use serde_json::Value;
use std::collections::HashMap;

use crate::types::{
    ConstrainedSampling, ConstrainedSamplingConfig, ConstrainedSamplingStrict, Tool,
};

/// `GrammarConstrainedSampling`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrammarConstrainedSampling {
    pub format: GrammarOutFormat,
    pub definition: String,
    pub input_property: String,
}

/// Output-side grammar format (`"lark" | "regex"` upstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrammarOutFormat {
    Lark,
    Regex,
}

impl GrammarOutFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Lark => "lark",
            Self::Regex => "regex",
        }
    }
}

/// `GrammarToolInputJsonBuffer`.
#[derive(Debug, Clone, Default)]
pub struct GrammarToolInputJsonBuffer {
    pub input: String,
    pub started: bool,
    pub closed: bool,
}

/// `getGrammarToolInput`.
pub fn get_grammar_tool_input(
    tool_name: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    input_property: &str,
) -> Result<String, String> {
    match arguments.get(input_property) {
        Some(serde_json::Value::String(input)) => Ok(input.clone()),
        _ => Err(format!(
            "Grammar tool call \"{tool_name}\" requires argument \"{input_property}\" to be a string."
        )),
    }
}

/// `appendGrammarToolInputJsonDelta`.
pub fn append_grammar_tool_input_json_delta(
    buffer: &mut GrammarToolInputJsonBuffer,
    input_property: &str,
    next_input: &str,
    close: bool,
) -> Result<Option<String>, String> {
    if buffer.closed {
        if close && next_input == buffer.input {
            return Ok(None);
        }
        return Err(format!(
            "grammar tool input for property \"{input_property}\" changed after it was closed"
        ));
    }
    if !next_input.starts_with(&buffer.input) {
        return Err(format!(
            "grammar tool input for property \"{input_property}\" changed non-monotonically"
        ));
    }

    let input_delta = &next_input[buffer.input.len()..];
    if !close && input_delta.is_empty() {
        return Ok(None);
    }

    let mut delta = String::new();
    if !buffer.started {
        delta.push('{');
        delta.push_str(&serde_json::to_string(input_property).unwrap_or_default());
        delta.push_str(":\"");
        buffer.started = true;
    }
    // JSON.stringify(inputDelta).slice(1, -1): escape without the quotes.
    let quoted = serde_json::to_string(input_delta).unwrap_or_default();
    if quoted.len() >= 2 {
        delta.push_str(&quoted[1..quoted.len() - 1]);
    }
    buffer.input = next_input.to_owned();

    if close {
        delta.push_str("\"}");
        buffer.closed = true;
    }
    Ok(Some(delta))
}

fn infer_grammar_input_property(tool: &Tool) -> Result<String, String> {
    let schema = &tool.parameters;
    if schema.get("type") != Some(&serde_json::Value::String("object".to_owned())) {
        return Err("grammar constrained sampling requires an object parameter schema".to_owned());
    }
    let required = schema.get("required").and_then(|r| r.as_array());
    let input_property =
        match required {
            Some(entries) if entries.len() == 1 => match entries[0].as_str() {
                Some(name) => name.to_owned(),
                None => return Err(
                    "grammar constrained sampling requires exactly one required string property"
                        .to_owned(),
                ),
            },
            _ => {
                return Err(
                    "grammar constrained sampling requires exactly one required string property"
                        .to_owned(),
                );
            }
        };

    let property = schema
        .get("properties")
        .and_then(|properties| properties.get(&input_property));
    match property {
        None => Err(format!(
            "grammar constrained sampling requires a properties entry for {input_property}"
        )),
        Some(property)
            if property.get("type") == Some(&serde_json::Value::String("string".to_owned())) =>
        {
            Ok(input_property)
        }
        Some(_) => Err(format!(
            "grammar constrained sampling property {input_property} must have type string"
        )),
    }
}

/// `UNSUPPORTED_STRICT_SCHEMA_KEYS` (constrained-sampling.ts:8-27).
const UNSUPPORTED_STRICT_SCHEMA_KEYS: &[&str] = &[
    "$ref",
    "$defs",
    "definitions",
    "allOf",
    "oneOf",
    "patternProperties",
    "dependentSchemas",
    "dependencies",
    "unevaluatedProperties",
    "propertyNames",
    "contains",
    "prefixItems",
    "not",
    "if",
    "then",
    "else",
];

/// `isStructuredSchema` — object/array-shaped schema (or a schema carrying
/// `properties`/`items`).
fn is_structured_schema(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    let types: Vec<&str> = match object.get("type") {
        Some(Value::String(single)) => vec![single.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => vec![],
    };
    types.contains(&"object")
        || types.contains(&"array")
        || object.contains_key("properties")
        || object.contains_key("items")
}

/// `schemaAllowsNull` — `type: "null"` (single or in a type union), a null
/// `const`/`enum` member, or an `anyOf` variant that allows null.
fn schema_allows_null(schema: &Value) -> bool {
    let Some(object) = schema.as_object() else {
        return false;
    };
    let type_allows_null = match object.get("type") {
        Some(Value::String(single)) => single == "null",
        Some(Value::Array(many)) => many.iter().any(|entry| entry == &serde_json::json!("null")),
        _ => false,
    };
    if type_allows_null {
        return true;
    }
    if object.get("const") == Some(&serde_json::json!(null)) {
        return true;
    }
    if let Some(Value::Array(variants)) = object.get("enum")
        && variants.contains(&serde_json::json!(null))
    {
        return true;
    }
    match object.get("anyOf") {
        Some(Value::Array(variants)) => variants.iter().any(schema_allows_null),
        _ => false,
    }
}

/// Returns true when a provider's strict mode rejects this schema keyword
/// with this value (`UnsupportedStrictSchemaKeywordCheck` @ 295cc72b0).
pub type UnsupportedStrictSchemaKeywordCheck = fn(&str, &Value) -> bool;

/// `makeJsonSchemaNodeStrict` (constrained-sampling.ts:60-102, `7915cdac6`;
/// provider keyword check @ 295cc72b0): in-place strict conversion of one
/// schema node. Error texts are the upstream
/// `UnsupportedStrictJsonSchemaError` messages verbatim.
fn make_json_schema_node_strict(
    schema: &mut Value,
    is_unsupported_keyword: Option<UnsupportedStrictSchemaKeywordCheck>,
) -> Result<(), String> {
    let Some(object) = schema.as_object_mut() else {
        return Err("boolean schemas are unsupported".to_owned());
    };
    for key in UNSUPPORTED_STRICT_SCHEMA_KEYS {
        // Upstream `schema[key] !== undefined` — mere presence fails.
        if object.contains_key(*key) {
            return Err(format!("{key} schemas are unsupported"));
        }
    }
    if let Some(is_unsupported) = is_unsupported_keyword {
        for (key, value) in object.iter() {
            if is_unsupported(key, value) {
                return Err(format!(
                    "{key}: {} is unsupported",
                    serde_json::to_string(value).unwrap_or_default()
                ));
            }
        }
    }

    if let Some(any_of) = object.get_mut("anyOf") {
        let Some(variants) = any_of.as_array_mut() else {
            return Err("anyOf must contain at least one schema".to_owned());
        };
        if variants.is_empty() {
            return Err("anyOf must contain at least one schema".to_owned());
        }
        for variant in variants.iter_mut() {
            if is_structured_schema(variant) {
                return Err("object and array unions are unsupported".to_owned());
            }
            make_json_schema_node_strict(variant, is_unsupported_keyword)?;
        }
    }

    if let Some(items) = object.get_mut("items") {
        if items.is_array() {
            return Err("tuple schemas are unsupported".to_owned());
        }
        make_json_schema_node_strict(items, is_unsupported_keyword)?;
    }

    let is_object_schema = object.get("type") == Some(&serde_json::json!("object"));
    if object.contains_key("properties") && !is_object_schema {
        return Err("properties require type object".to_owned());
    }
    if !is_object_schema {
        return Ok(());
    }
    if let Some(additional) = object.get("additionalProperties")
        && additional != &serde_json::json!(false)
    {
        return Err("schema-valued or true additionalProperties is unsupported".to_owned());
    }
    if let Some(properties) = object.get("properties")
        && !properties.is_object()
    {
        return Err("object properties must be a schema map".to_owned());
    }
    if let Some(required) = object.get("required") {
        let Some(entries) = required.as_array() else {
            return Err("object required must be a string array".to_owned());
        };
        if entries.iter().any(|entry| !entry.is_string()) {
            return Err("object required must be a string array".to_owned());
        }
    }

    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let property_names: Vec<String> = properties.keys().cloned().collect();
    let required: Vec<String> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    if required.iter().any(|key| !property_names.contains(key)) {
        return Err("required contains an unknown property".to_owned());
    }
    for key in &property_names {
        let Some(property) = object
            .get_mut("properties")
            .and_then(Value::as_object_mut)
            .and_then(|properties| properties.get_mut(key))
        else {
            continue;
        };
        make_json_schema_node_strict(property, is_unsupported_keyword)?;
        if !required.contains(key) && !schema_allows_null(property) {
            *property = serde_json::json!({
                "anyOf": [property.clone(), {"type": "null"}],
            });
        }
    }
    object.insert("required".to_owned(), serde_json::json!(property_names));
    object.insert("additionalProperties".to_owned(), serde_json::json!(false));
    Ok(())
}

/// `makeStrictJsonSchema` (:117-130, `7915cdac6`): clone, strict-convert the
/// root, require a `type: "object"` root.
pub fn make_strict_json_schema(schema: &Value) -> Result<Value, String> {
    make_strict_json_schema_with_check(schema, None)
}

/// [`make_strict_json_schema`] with a per-provider rejected-keyword check
/// (295cc72b0): "prefer" tools that hit it fall back to non-strict.
pub fn make_strict_json_schema_with_check(
    schema: &Value,
    is_unsupported_keyword: Option<UnsupportedStrictSchemaKeywordCheck>,
) -> Result<Value, String> {
    let mut cloned = schema.clone();
    if !cloned.is_object() {
        return Err("root schema must have type object".to_owned());
    }
    make_json_schema_node_strict(&mut cloned, is_unsupported_keyword)?;
    if cloned.get("type") != Some(&serde_json::json!("object")) {
        return Err("root schema must have type object".to_owned());
    }
    Ok(cloned)
}

/// `getJsonSchemaToolParameters` (:133-135): the strict-converted parameters
/// when `strict`, the original otherwise.
pub fn get_json_schema_tool_parameters(tool: &Tool, strict: Option<bool>) -> Result<Value, String> {
    if strict == Some(true) {
        make_strict_json_schema(&tool.parameters)
    } else {
        Ok(tool.parameters.clone())
    }
}

/// `resolveJsonSchemaStrictSampling` (:214-222 @ 295cc72b0): `None` = the
/// tool keeps its unconverted schema; `Some(true)` = strict conversion;
/// `prefer` falls back to non-strict for unsupported constructs, `require`
/// fails loudly.
pub fn resolve_json_schema_strict_sampling(
    tool: &Tool,
    supports_strict_mode: bool,
) -> Result<Option<bool>, String> {
    resolve_json_schema_strict_sampling_with_check(tool, supports_strict_mode, None)
}

/// [`resolve_json_schema_strict_sampling`] with a per-provider rejected
/// keyword check (295cc72b0).
pub fn resolve_json_schema_strict_sampling_with_check(
    tool: &Tool,
    supports_strict_mode: bool,
    is_unsupported_keyword: Option<UnsupportedStrictSchemaKeywordCheck>,
) -> Result<Option<bool>, String> {
    let strict = match &tool.constrained_sampling {
        Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema { strict })) => {
            *strict
        }
        _ => return Ok(None),
    };

    if supports_strict_mode {
        // Try the strict conversion: unsupported constructs fall back for
        // `prefer` and fail loudly for `require` (:214-222, `7915cdac6`).
        return match make_strict_json_schema_with_check(&tool.parameters, is_unsupported_keyword) {
            Ok(_) => Ok(Some(true)),
            Err(_reason) if strict != ConstrainedSamplingStrict::Require => Ok(None),
            Err(reason) => Err(format!(
                "Tool \"{}\" requires JSON-schema constrained sampling, but {reason}.",
                tool.name
            )),
        };
    }
    if strict == ConstrainedSamplingStrict::Require {
        return Err(format!(
            "Tool \"{}\" requires JSON-schema constrained sampling, but strict tools are unsupported.",
            tool.name
        ));
    }
    Ok(None)
}

/// `resolveGrammarConstrainedSampling`.
pub fn resolve_grammar_constrained_sampling(
    tool: &Tool,
    supports_open_ai_grammar_tools: bool,
) -> Result<Option<GrammarConstrainedSampling>, String> {
    let variants = match &tool.constrained_sampling {
        Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::Grammar { variants })) => {
            variants
        }
        _ => return Ok(None),
    };

    if !supports_open_ai_grammar_tools {
        return Ok(None);
    }

    let lark_definition = variants.openai_lark.as_deref();
    let regex_definition = variants.openai_regex.as_deref();
    let has_lark_definition = lark_definition.is_some_and(|d| !d.trim().is_empty());
    let has_regex_definition = regex_definition.is_some_and(|d| !d.trim().is_empty());
    if !has_lark_definition && !has_regex_definition {
        return Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: no supported grammar variant was provided.",
            tool.name
        ));
    }

    let (format, definition) = if has_lark_definition {
        // invariant: has_lark_definition implies openai_lark is Some
        (
            GrammarOutFormat::Lark,
            lark_definition.unwrap_or_default().to_owned(),
        )
    } else {
        // invariant: !has_lark && has_regex implies openai_regex is Some
        (
            GrammarOutFormat::Regex,
            regex_definition.unwrap_or_default().to_owned(),
        )
    };

    match infer_grammar_input_property(tool) {
        Ok(input_property) => Ok(Some(GrammarConstrainedSampling {
            format,
            definition,
            input_property,
        })),
        Err(message) => Err(format!(
            "Tool \"{}\" cannot use grammar constrained sampling: {message}.",
            tool.name
        )),
    }
}

/// `createGrammarToolInputProperties`: tool name → grammar input property.
pub fn create_grammar_tool_input_properties(
    tools: Option<&[Tool]>,
    supports_open_ai_grammar_tools: bool,
) -> Result<HashMap<String, String>, String> {
    let mut properties = HashMap::new();
    for tool in tools.unwrap_or(&[]) {
        if let Some(grammar) =
            resolve_grammar_constrained_sampling(tool, supports_open_ai_grammar_tools)?
        {
            properties.insert(tool.name.clone(), grammar.input_property);
        }
    }
    Ok(properties)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn tool(constrained_sampling: serde_json::Value) -> Tool {
        serde_json::from_value(json!({
            "name": "t", "description": "d",
            "parameters": {"type": "object", "properties": {}, "required": []},
            "constrainedSampling": constrained_sampling,
        }))
        .expect("tool")
    }

    #[test]
    fn test_resolve_json_schema_strict_sampling() {
        let prefer = tool(json!({"type": "json_schema", "strict": "prefer"}));
        let require = tool(json!({"type": "json_schema", "strict": "require"}));
        let none = Tool {
            constrained_sampling: None,
            ..prefer.clone()
        };

        assert_eq!(resolve_json_schema_strict_sampling(&none, true), Ok(None));
        assert_eq!(
            resolve_json_schema_strict_sampling(&prefer, true),
            Ok(Some(true))
        );
        assert_eq!(
            resolve_json_schema_strict_sampling(&prefer, false),
            Ok(None)
        );
        assert_eq!(
            resolve_json_schema_strict_sampling(&require, true),
            Ok(Some(true))
        );
        assert_eq!(
            resolve_json_schema_strict_sampling(&require, false),
            Err("Tool \"t\" requires JSON-schema constrained sampling, but strict tools are unsupported.".to_owned())
        );
    }

    /// `makeStrictJsonSchema` (7915cdac6 / constrained-sampling.test.ts):
    /// all properties promoted to required; optional non-nullable properties
    /// wrapped in `anyOf: [schema, {type:"null"}]`; nullable ones left
    /// unwrapped; `additionalProperties: false`.
    #[test]
    fn test_make_strict_json_schema() {
        let schema = json!({
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "offset": {"type": "number"},
                "nullable": {"anyOf": [{"type": "string"}, {"type": "null"}]}
            },
            "required": ["path"]
        });
        let strict = make_strict_json_schema(&schema).expect("strict");
        assert_eq!(
            strict,
            json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string"},
                    "offset": {"anyOf": [{"type": "number"}, {"type": "null"}]},
                    "nullable": {"anyOf": [{"type": "string"}, {"type": "null"}]}
                },
                "required": ["path", "offset", "nullable"],
                "additionalProperties": false
            })
        );
        // Nested objects recurse; arrays strictify their (non-tuple) items.
        let schema = json!({
            "type": "object",
            "properties": {
                "nested": {
                    "type": "object",
                    "properties": {"flag": {"type": "boolean"}},
                    "required": ["flag"]
                },
                "list": {"type": "array", "items": {"type": "string"}}
            }
        });
        let strict = make_strict_json_schema(&schema).expect("strict");
        // `nested` and `list` are optional and non-nullable → strictified
        // first, then wrapped in `anyOf` (the strict form rides in arm 0).
        assert_eq!(
            strict["properties"]["nested"]["anyOf"][0]["additionalProperties"],
            json!(false)
        );
        assert_eq!(
            strict["properties"]["nested"]["anyOf"][0]["required"],
            json!(["flag"])
        );
        // The array items schema itself stays a plain string schema.
        assert_eq!(
            strict["properties"]["list"]["anyOf"][0]["items"],
            json!({"type": "string"})
        );
        // Input schema is not mutated.
        assert!(
            schema["properties"]["nested"]
                .get("additionalProperties")
                .is_none()
        );
    }

    /// Unsupported constructs: `$ref` / tuple items / object unions error
    /// with the upstream message texts; `prefer` falls back to non-strict
    /// and `require` fails with the tool-name wrapper (resolve path).
    #[test]
    fn test_make_strict_json_schema_unsupported() {
        for (schema, message) in [
            (
                json!({"type": "object", "properties": {"a": {"$ref": "#/$defs/a"}}}),
                "$ref schemas are unsupported",
            ),
            (
                json!({"type": "object", "properties": {"a": {"type": "array", "items": [{"type": "string"}]}}}),
                "tuple schemas are unsupported",
            ),
            (
                json!({"type": "object", "properties": {"a": {"anyOf": [{"type": "object", "properties": {}}, {"type": "string"}]}}}),
                "object and array unions are unsupported",
            ),
            (
                json!({"type": "object", "oneOf": []}),
                "oneOf schemas are unsupported",
            ),
            (
                json!({"type": "object", "properties": {"a": true}}),
                "boolean schemas are unsupported",
            ),
        ] {
            assert_eq!(
                make_strict_json_schema(&schema),
                Err(message.to_owned()),
                "{schema}"
            );
        }

        // `resolveJsonSchemaStrictSampling`: prefer falls back, require
        // fails with the wrapper (constrained-sampling.ts:214-222).
        let prefer = serde_json::from_value::<Tool>(json!({
            "name": "t", "description": "d",
            "parameters": {"type": "object", "properties": {"a": {"$ref": "#/$defs/a"}}},
            "constrainedSampling": {"type": "json_schema", "strict": "prefer"},
        }))
        .expect("tool");
        assert_eq!(resolve_json_schema_strict_sampling(&prefer, true), Ok(None));
        let require = serde_json::from_value::<Tool>(json!({
            "name": "t", "description": "d",
            "parameters": {"type": "object", "properties": {"a": {"$ref": "#/$defs/a"}}},
            "constrainedSampling": {"type": "json_schema", "strict": "require"},
        }))
        .expect("tool");
        assert_eq!(
            resolve_json_schema_strict_sampling(&require, true),
            Err("Tool \"t\" requires JSON-schema constrained sampling, but $ref schemas are unsupported.".to_owned())
        );
        // A supported schema under `require` still resolves strict.
        let fine = serde_json::from_value::<Tool>(json!({
            "name": "t", "description": "d",
            "parameters": {"type": "object", "properties": {"a": {"type": "string"}}},
            "constrainedSampling": {"type": "json_schema", "strict": "require"},
        }))
        .expect("tool");
        assert_eq!(
            resolve_json_schema_strict_sampling(&fine, true),
            Ok(Some(true))
        );
    }

    /// `getJsonSchemaToolParameters`: pass-through unless strict.
    #[test]
    fn test_get_json_schema_tool_parameters() {
        let tool = serde_json::from_value::<Tool>(json!({
            "name": "t", "description": "d",
            "parameters": {"type": "object", "properties": {"a": {"type": "string"}}},
        }))
        .expect("tool");
        assert_eq!(
            get_json_schema_tool_parameters(&tool, None).expect("params"),
            tool.parameters
        );
        assert_eq!(
            get_json_schema_tool_parameters(&tool, Some(false)).expect("params"),
            tool.parameters
        );
        let strict = get_json_schema_tool_parameters(&tool, Some(true)).expect("params");
        assert_eq!(strict["required"], json!(["a"]));
        assert_eq!(strict["additionalProperties"], json!(false));
    }

    #[test]
    fn test_append_grammar_tool_input_json_delta() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        // No-op delta before close yields None.
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "input", "", false),
            Ok(None)
        );
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "input", "hel", false),
            Ok(Some("{\"input\":\"hel".to_owned()))
        );
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "input", "hello", true),
            Ok(Some("lo\"}".to_owned()))
        );
        assert!(buffer.closed);
        // Idempotent close after closed is a no-op.
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "input", "hello", true),
            Ok(None)
        );
    }

    #[test]
    fn test_append_grammar_tool_input_json_delta_non_monotonic() {
        let mut buffer = GrammarToolInputJsonBuffer::default();
        append_grammar_tool_input_json_delta(&mut buffer, "input", "abc", false).expect("delta");
        assert_eq!(
            append_grammar_tool_input_json_delta(&mut buffer, "input", "abx", false),
            Err("grammar tool input for property \"input\" changed non-monotonically".to_owned())
        );
    }

    #[test]
    fn test_resolve_grammar_constrained_sampling() {
        let grammar_tool: Tool = serde_json::from_value(json!({
            "name": "g", "description": "d",
            "parameters": {
                "type": "object",
                "properties": {"input": {"type": "string"}},
                "required": ["input"]
            },
            "constrainedSampling": {"type": "grammar", "variants": {"openai_lark": "start: x"}}
        }))
        .expect("tool");

        // Unsupported provider → None.
        assert_eq!(
            resolve_grammar_constrained_sampling(&grammar_tool, false),
            Ok(None)
        );
        let grammar = resolve_grammar_constrained_sampling(&grammar_tool, true)
            .expect("ok")
            .expect("grammar");
        assert_eq!(grammar.format, GrammarOutFormat::Lark);
        assert_eq!(grammar.definition, "start: x");
        assert_eq!(grammar.input_property, "input");
    }

    #[test]
    fn test_get_grammar_tool_input() {
        let mut arguments = serde_json::Map::new();
        arguments.insert("input".to_owned(), json!("value"));
        assert_eq!(
            get_grammar_tool_input("t", &arguments, "input"),
            Ok("value".to_owned())
        );
        arguments.insert("input".to_owned(), json!(42));
        assert_eq!(
            get_grammar_tool_input("t", &arguments, "input"),
            Err("Grammar tool call \"t\" requires argument \"input\" to be a string.".to_owned())
        );
    }
}
