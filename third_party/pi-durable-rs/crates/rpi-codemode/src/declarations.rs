//! TypeScript declaration rendering for the script-visible API (port of
//! `packages/codemode/src/declarations.ts` @ abe508e1b).
//!
//! Tools become members of `declare const tools`, globals become
//! `declare function` statements, and `ns.member` globals members of
//! `declare const ns`. Descriptions become doc comments; schemas become
//! types.

use serde_json::Value;

use crate::identifier::to_codemode_identifier;
use crate::types::CodemodeToolInfo;

const INDENT: &str = "  ";
/// Largest rendered input type, in characters, before it becomes `unknown`
/// (declarations.ts:10).
pub const DEFAULT_INPUT_SCHEMA_MAX_CHARS: usize = 16_000;
/// Local `$ref` expansions per rendered schema (declarations.ts:12).
const MAX_REF_EXPANSIONS: usize = 32;

/// TypeScript types for MCP results, from the MCP `CallToolResult` schema,
/// so `CallToolResult<T>` declarations can refer to them
/// (declarations.ts:18-93).
pub const MCP_TYPESCRIPT_PREAMBLE: &str = r#"type Role = "user" | "assistant";
type MetaObject = Record<string, unknown>;
type Annotations = {
  audience?: Role[];
  priority?: number;
  lastModified?: string;
};
type Icon = {
  src: string;
  mimeType?: string;
  sizes?: string[];
  theme?: "light" | "dark";
};
type TextResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  text: string;
};
type BlobResourceContents = {
  uri: string;
  mimeType?: string;
  _meta?: MetaObject;
  blob: string;
};
type TextContent = {
  type: "text";
  text: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ImageContent = {
  type: "image";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type AudioContent = {
  type: "audio";
  data: string;
  mimeType: string;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ResourceLink = {
  icons?: Icon[];
  name: string;
  title?: string;
  uri: string;
  description?: string;
  mimeType?: string;
  annotations?: Annotations;
  size?: number;
  _meta?: MetaObject;
  type: "resource_link";
};
type EmbeddedResource = {
  type: "resource";
  resource: TextResourceContents | BlobResourceContents;
  annotations?: Annotations;
  _meta?: MetaObject;
};
type ContentBlock =
  | TextContent
  | ImageContent
  | AudioContent
  | ResourceLink
  | EmbeddedResource;
type CallToolResult<TStructured = { [key: string]: unknown }> = {
  _meta?: MetaObject;
  content: ContentBlock[];
  isError?: boolean;
  structuredContent?: TStructured;
  [key: string]: unknown;
};"#;

/// `RenderDeclarationsOptions` (declarations.ts:95-98) collapsed to the two
/// slices.
#[derive(Default)]
pub struct RenderDeclarationsOptions<'a> {
    pub tools: &'a [CodemodeToolInfo],
    pub globals: &'a [CodemodeToolInfo],
}

/// Render TypeScript declarations for the script-visible API.
pub fn render_declarations(options: RenderDeclarationsOptions<'_>) -> String {
    let mut sections: Vec<String> = Vec::new();
    if !options.tools.is_empty() {
        let members = options
            .tools
            .iter()
            .map(|tool| {
                format!(
                    "{}{INDENT}{}",
                    doc_comment(tool.description.as_deref(), INDENT),
                    render_tool_signature(tool, None)
                )
            })
            .collect::<Vec<_>>();
        sections.push(format!(
            "declare const tools: {{\n{}\n}};",
            members.join("\n")
        ));
    }
    // Insertion-ordered namespaces (`Map` upstream).
    let mut namespace_names: Vec<String> = Vec::new();
    let mut namespaces: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for global in options.globals {
        match global.name.find('.') {
            None => {
                sections.push(render_global(
                    &format!("declare function {}", global.name),
                    global,
                    "",
                ));
            }
            Some(dot) => {
                let namespace = global.name[..dot].to_owned();
                let member = render_global(&global.name[dot + 1..], global, INDENT);
                let members = namespaces.entry(namespace.clone()).or_insert_with(|| {
                    namespace_names.push(namespace.clone());
                    Vec::new()
                });
                members.push(member);
            }
        }
    }
    for namespace in namespace_names {
        let members = namespaces.remove(&namespace).unwrap_or_default();
        sections.push(format!(
            "declare const {namespace}: {{\n{}\n}};",
            members.join("\n")
        ));
    }
    sections.join("\n\n")
}

/// One tool as a member of the `tools` object:
/// `name(args: T): Promise<R>;` with the name as the identifier scripts use.
/// Input types longer than `input_max_chars` render as `unknown`.
pub fn render_tool_signature(tool: &CodemodeToolInfo, input_max_chars: Option<usize>) -> String {
    let input = match &tool.input_schema {
        None => "unknown".to_owned(),
        Some(schema) => schema_to_type(
            schema,
            Some(input_max_chars.unwrap_or(DEFAULT_INPUT_SCHEMA_MAX_CHARS)),
        ),
    };
    format!(
        "{}(args: {}): Promise<{}>;",
        to_codemode_identifier(&tool.name),
        input,
        render_tool_output_type(tool.output_schema.as_ref())
    )
}

/// A tool's sample: the description followed by the tool's declaration
/// (declarations.ts:149-159).
pub fn render_tool_sample(tool: &CodemodeToolInfo) -> String {
    let declaration = format!(
        "declare const tools: {{ {} }};",
        render_tool_signature(tool, None)
    );
    format!(
        "{}\n\ncodemode tool declaration:\n```ts\n{declaration}\n```",
        tool.description.as_deref().unwrap_or("").trim()
    )
}

/// The `structuredContent` schema of an MCP `CallToolResult` output schema
/// (detected by a `content` array of objects, boolean `isError`, and object
/// `_meta`), `Some(true)` when it declares none, or `None` when the schema
/// is not a `CallToolResult` (declarations.ts:161-175).
pub fn mcp_structured_content_schema(schema: Option<&Value>) -> Option<Value> {
    let schema = schema?;
    let schema = schema.as_object()?;
    let properties = schema.get("properties").and_then(Value::as_object)?;
    let content = properties.get("content").and_then(Value::as_object)?;
    if content.get("type").and_then(Value::as_str) != Some("array") {
        return None;
    }
    if content
        .get("items")
        .and_then(Value::as_object)
        .and_then(|items| items.get("type"))
        .and_then(Value::as_str)
        != Some("object")
    {
        return None;
    }
    if properties
        .get("isError")
        .and_then(Value::as_object)
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        != Some("boolean")
    {
        return None;
    }
    if properties
        .get("_meta")
        .and_then(Value::as_object)
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        != Some("object")
    {
        return None;
    }
    // Upstream: `isObject(structuredContent) || typeof structuredContent
    // === "boolean" ? structuredContent : true` — anything else falls back
    // to the `true` (no declared shape) sentinel.
    match properties.get("structuredContent") {
        Some(value) if value.is_object() || value.is_boolean() => Some(value.clone()),
        _ => Some(Value::Bool(true)),
    }
}

/// The type a tool call resolves to: `CallToolResult<T>` for MCP output
/// schemas, the schema's type otherwise, and `unknown` without a schema.
pub fn render_tool_output_type(schema: Option<&Value>) -> String {
    if let Some(structured) = mcp_structured_content_schema(schema) {
        let rendered = schema_to_type(&structured, None);
        return if rendered == "unknown" {
            "CallToolResult".to_owned()
        } else {
            format!("CallToolResult<{rendered}>")
        };
    }
    match schema {
        None => "unknown".to_owned(),
        Some(schema) => schema_to_type(schema, None),
    }
}

fn render_global(head: &str, global: &CodemodeToolInfo, indent: &str) -> String {
    if let Some(signature) = &global.signature {
        return format!(
            "{}{indent}{head}{signature};",
            doc_comment(global.description.as_deref(), indent)
        );
    }
    let input = match &global.input_schema {
        None => "unknown".to_owned(),
        Some(schema) => schema_to_type(schema, None),
    };
    let output = match &global.output_schema {
        None => "unknown".to_owned(),
        Some(schema) => schema_to_type(schema, None),
    };
    format!(
        "{}{indent}{head}(args: {input}): Promise<{output}>;",
        doc_comment(global.description.as_deref(), indent)
    )
}

fn doc_comment(description: Option<&str>, indent: &str) -> String {
    let Some(text) = description.map(str::trim).filter(|text| !text.is_empty()) else {
        return String::new();
    };
    let text = text.replace("*/", "*\\/");
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() == 1 {
        return format!("{indent}/** {} */\n", lines[0]);
    }
    let body = lines
        .iter()
        .map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.is_empty() {
                format!("{indent} *")
            } else {
                format!("{indent} * {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{indent}/**\n{body}\n{indent} */\n")
}

fn property_key(name: &str) -> String {
    if is_identifier(name) {
        name.to_owned()
    } else {
        serde_json::to_string(name).unwrap_or_else(|_| format!("\"{name}\""))
    }
}

fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' || first == '$' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
}

fn union(types: Vec<String>) -> String {
    let mut unique: Vec<String> = Vec::new();
    for item in types {
        if !unique.contains(&item) {
            unique.push(item);
        }
    }
    if unique.iter().any(|item| item == "unknown") {
        return "unknown".to_owned();
    }
    if unique.is_empty() {
        return "never".to_owned();
    }
    unique.join(" | ")
}

/// Convert a JSON Schema to a TypeScript type expression
/// (declarations.ts:221-308).
pub fn schema_to_type(schema: &Value, max_chars: Option<usize>) -> String {
    let mut context = SchemaContext {
        root: schema,
        resolving: Vec::new(),
        expansions: 0,
    };
    let rendered = to_type(schema, &mut context);
    match max_chars {
        Some(max) if rendered.chars().count() > max => "unknown".to_owned(),
        _ => rendered,
    }
}

struct SchemaContext<'a> {
    root: &'a Value,
    resolving: Vec<String>,
    expansions: usize,
}

fn is_object(value: &Value) -> bool {
    value.is_object()
}

fn resolve_ref(reference: &str, root: &Value) -> Option<Value> {
    if reference != "#" && !reference.starts_with("#/") {
        return None;
    }
    let mut current = root;
    for segment in reference[2..].split('/').filter(|part| !part.is_empty()) {
        let key = segment.replace("~1", "/").replace("~0", "~");
        current = current.as_object()?.get(&key)?;
    }
    if current.is_boolean() || current.is_object() {
        Some(current.clone())
    } else {
        None
    }
}

fn to_type(schema: &Value, context: &mut SchemaContext<'_>) -> String {
    if schema.is_boolean() {
        return if schema.as_bool() == Some(true) {
            "unknown".to_owned()
        } else {
            "never".to_owned()
        };
    }
    let Some(object) = schema.as_object() else {
        return "unknown".to_owned();
    };
    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        if context.resolving.iter().any(|item| item == reference)
            || context.expansions >= MAX_REF_EXPANSIONS
        {
            return "unknown".to_owned();
        }
        let Some(target) = resolve_ref(reference, context.root) else {
            return "unknown".to_owned();
        };
        context.expansions += 1;
        context.resolving.push(reference.to_owned());
        let rendered = to_type(&target, context);
        context.resolving.pop();
        return rendered;
    }

    if object.contains_key("const") {
        return serde_json::to_string(&object["const"]).unwrap_or_else(|_| "unknown".to_owned());
    }
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return union(
            values
                .iter()
                .map(|value| serde_json::to_string(value).unwrap_or_else(|_| "unknown".to_owned()))
                .collect(),
        );
    }

    let variants = object
        .get("anyOf")
        .and_then(Value::as_array)
        .or_else(|| object.get("oneOf").and_then(Value::as_array));
    if let Some(variants) = variants {
        return union(
            variants
                .iter()
                .map(|variant| to_type(variant, context))
                .collect(),
        );
    }
    if let Some(parts) = object.get("allOf").and_then(Value::as_array) {
        let parts: Vec<String> = parts
            .iter()
            .map(|part| to_type(part, context))
            .filter(|part| part != "unknown")
            .collect();
        if parts.is_empty() {
            return "unknown".to_owned();
        }
        return parts
            .iter()
            .map(|part| {
                if part.contains(" | ") {
                    format!("({part})")
                } else {
                    part.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" & ");
    }

    let schema_type = object.get("type");
    if let Some(types) = schema_type.and_then(Value::as_array) {
        return union(
            types
                .iter()
                .map(|entry| {
                    let mut clone = object.clone();
                    clone.insert("type".to_owned(), entry.clone());
                    to_type(&Value::Object(clone), context)
                })
                .collect(),
        );
    }
    match schema_type.and_then(Value::as_str) {
        Some("string") => "string".to_owned(),
        Some("number") | Some("integer") => "number".to_owned(),
        Some("boolean") => "boolean".to_owned(),
        Some("null") => "null".to_owned(),
        Some("array") => array_type(object, context),
        Some("object") => object_type(object, context),
        None => {
            if object.contains_key("properties")
                || object.contains_key("additionalProperties")
                || object.contains_key("required")
            {
                return object_type(object, context);
            }
            if object.contains_key("items") || object.contains_key("prefixItems") {
                return array_type(object, context);
            }
            "unknown".to_owned()
        }
        Some(_) => "unknown".to_owned(),
    }
}

fn array_type(object: &serde_json::Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    if let Some(items) = object.get("items")
        && !items.is_array()
    {
        return format!("Array<{}>", to_type(items, context));
    }
    let tuple: Vec<Value> = object
        .get("prefixItems")
        .and_then(Value::as_array)
        .or_else(|| object.get("items").and_then(Value::as_array))
        .cloned()
        .unwrap_or_default();
    if !tuple.is_empty() {
        return format!(
            "[{}]",
            tuple
                .iter()
                .map(|item| to_type(item, context))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    "unknown[]".to_owned()
}

fn description_of(property: &Value) -> String {
    property
        .as_object()
        .and_then(|object| object.get("description"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .to_owned()
}

fn object_type(object: &serde_json::Map<String, Value>, context: &mut SchemaContext<'_>) -> String {
    let properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let required: Vec<String> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let mut names: Vec<String> = properties.keys().cloned().collect();
    names.sort();
    let mut members: Vec<String> = names
        .iter()
        .map(|name| {
            let optional = if required.contains(name) { "" } else { "?" };
            format!(
                "{}{optional}: {};",
                property_key(name),
                to_type(&properties[name], context)
            )
        })
        .collect();
    let additional = object.get("additionalProperties");
    match additional {
        Some(value) if value != &Value::Bool(false) => {
            let rendered = if value == &Value::Bool(true) {
                "unknown".to_owned()
            } else {
                to_type(value, context)
            };
            members.push(format!("[key: string]: {rendered};"));
        }
        None if names.is_empty() => {
            members.push("[key: string]: unknown;".to_owned());
        }
        _ => {}
    }
    if members.is_empty() {
        return "{}".to_owned();
    }
    if !names
        .iter()
        .any(|name| !description_of(&properties[name]).is_empty())
    {
        return format!("{{ {} }}", members.join(" "));
    }

    let mut lines: Vec<String> = vec!["{".to_owned()];
    for (index, name) in names.iter().enumerate() {
        let description = description_of(&properties[name]);
        for line in description.split('\n') {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if !line.trim().is_empty() {
                lines.push(format!("{INDENT}// {}", line.trim()));
            }
        }
        lines.push(format!(
            "{INDENT}{}",
            members[index].replace('\n', &format!("\n{INDENT}"))
        ));
    }
    for member in members.iter().skip(names.len()) {
        lines.push(format!("{INDENT}{member}"));
    }
    lines.push("}".to_owned());
    lines.join("\n")
}

/// Whether a value is a plain object (the upstream `isObject` helper).
pub fn is_plain_object(value: &Value) -> bool {
    is_object(value)
}

#[cfg(test)]
mod tests {
    //! Ports `packages/codemode/test/declarations.test.ts` @ abe508e1b.

    use super::*;
    use serde_json::json;

    fn mcp_result_schema(structured_content: Option<Value>) -> Value {
        let mut properties = serde_json::Map::new();
        properties.insert(
            "content".to_owned(),
            json!({ "type": "array", "items": { "type": "object" } }),
        );
        if let Some(structured) = structured_content {
            properties.insert("structuredContent".to_owned(), structured);
        }
        properties.insert("isError".to_owned(), json!({ "type": "boolean" }));
        properties.insert("_meta".to_owned(), json!({ "type": "object" }));
        json!({
            "type": "object",
            "properties": properties,
            "required": ["content"],
        })
    }

    #[test]
    fn renders_primitives_literals_and_unions() {
        assert_eq!(schema_to_type(&json!({ "type": "string" }), None), "string");
        assert_eq!(
            schema_to_type(&json!({ "type": "integer" }), None),
            "number"
        );
        assert_eq!(
            schema_to_type(&json!({ "type": ["string", "null"] }), None),
            "string | null"
        );
        assert_eq!(schema_to_type(&json!({ "const": "a" }), None), "\"a\"");
        assert_eq!(
            schema_to_type(&json!({ "enum": ["a", 1, null] }), None),
            "\"a\" | 1 | null"
        );
        assert_eq!(
            schema_to_type(
                &json!({ "anyOf": [{ "type": "string" }, { "type": "number" }] }),
                None
            ),
            "string | number"
        );
        assert_eq!(
            schema_to_type(&json!({ "anyOf": [{ "type": "string" }, {}] }), None),
            "unknown"
        );
        assert_eq!(
            schema_to_type(
                &json!({ "allOf": [{ "anyOf": [{ "type": "string" }, { "type": "number" }] }, { "const": 1 }] }),
                None
            ),
            "(string | number) & 1"
        );
        assert_eq!(
            schema_to_type(&json!({ "$ref": "#/defs/x" }), None),
            "unknown"
        );
        assert_eq!(schema_to_type(&json!(true), None), "unknown");
        assert_eq!(schema_to_type(&json!(false), None), "never");
    }

    #[test]
    fn renders_objects_on_one_line_with_sorted_properties() {
        assert_eq!(
            schema_to_type(
                &json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" }, "max-lines": { "type": "number" } },
                    "required": ["city"],
                    "additionalProperties": false,
                }),
                None
            ),
            "{ city: string; \"max-lines\"?: number; }"
        );
        assert_eq!(
            schema_to_type(
                &json!({ "type": "object", "additionalProperties": { "type": "number" } }),
                None
            ),
            "{ [key: string]: number; }"
        );
        assert_eq!(
            schema_to_type(&json!({ "type": "object" }), None),
            "{ [key: string]: unknown; }"
        );
        assert_eq!(
            schema_to_type(
                &json!({ "type": "object", "properties": {}, "additionalProperties": false }),
                None
            ),
            "{}"
        );
    }

    #[test]
    fn puts_property_descriptions_on_comment_lines() {
        assert_eq!(
            schema_to_type(
                &json!({
                    "type": "object",
                    "properties": {
                        "weather": {
                            "type": "array",
                            "description": "look up weather for a given list of locations",
                            "items": { "type": "object", "properties": { "location": { "type": "string" } }, "required": ["location"] }
                        }
                    },
                    "required": ["weather"]
                }),
                None
            ),
            "{\n  // look up weather for a given list of locations\n  weather: Array<{ location: string; }>;\n}"
        );
        assert_eq!(
            schema_to_type(
                &json!({
                    "type": "object",
                    "properties": {
                        "outer": {
                            "type": "object",
                            "description": "Outer",
                            "properties": { "inner": { "type": "string", "description": "Inner" } }
                        }
                    }
                }),
                None
            ),
            "{\n  // Outer\n  outer?: {\n    // Inner\n    inner?: string;\n  };\n}"
        );
    }

    #[test]
    fn resolves_local_references_and_stops_at_recursive_ones() {
        let schema = json!({
            "type": "object",
            "properties": {
                "item": { "$ref": "#/$defs/Item" },
                "legacy": { "$ref": "#/definitions/Legacy" },
                "remote": { "$ref": "https://example.com/schema.json" }
            },
            "required": ["item"],
            "$defs": {
                "Item": {
                    "type": "object",
                    "properties": { "id": { "type": "string" }, "parent": { "$ref": "#/$defs/Item" } },
                    "required": ["id"]
                }
            },
            "definitions": { "Legacy": { "enum": ["a", "b"] } }
        });
        assert_eq!(
            schema_to_type(&schema, None),
            "{ item: { id: string; parent?: unknown; }; legacy?: \"a\" | \"b\"; remote?: unknown; }"
        );
    }

    #[test]
    fn renders_arrays_and_tuples() {
        assert_eq!(
            schema_to_type(
                &json!({ "type": "array", "items": { "type": "string" } }),
                None
            ),
            "Array<string>"
        );
        assert_eq!(
            schema_to_type(
                &json!({ "type": "array", "prefixItems": [{ "type": "string" }, { "type": "number" }] }),
                None
            ),
            "[string, number]"
        );
        assert_eq!(
            schema_to_type(&json!({ "type": "array" }), None),
            "unknown[]"
        );
    }

    #[test]
    fn renders_types_over_the_budget_as_unknown() {
        let properties: serde_json::Map<String, Value> = (0..50)
            .map(|index| (format!("field{index}"), json!({ "type": "string" })))
            .collect();
        let schema = json!({ "type": "object", "properties": properties });
        assert_eq!(schema_to_type(&schema, Some(100)), "unknown");
        assert!(schema_to_type(&schema, None).contains("field49?: string;"));
    }

    #[test]
    fn renders_signatures_with_normalized_identifiers() {
        assert_eq!(
            render_tool_signature(
                &CodemodeToolInfo {
                    name: "hidden-dynamic-tool".to_owned(),
                    input_schema: Some(json!({
                        "type": "object",
                        "properties": { "city": { "type": "string" } },
                        "required": ["city"],
                        "additionalProperties": false
                    })),
                    output_schema: Some(
                        json!({ "type": "object", "properties": { "ok": { "type": "boolean" } }, "required": ["ok"] })
                    ),
                    ..Default::default()
                },
                None
            ),
            "hidden_dynamic_tool(args: { city: string; }): Promise<{ ok: boolean; }>;"
        );
        assert_eq!(
            render_tool_signature(
                &CodemodeToolInfo {
                    name: "free".to_owned(),
                    ..Default::default()
                },
                None
            ),
            "free(args: unknown): Promise<unknown>;"
        );
    }

    #[test]
    fn renders_mcp_call_tool_result_output_schemas() {
        let input_schema =
            json!({ "type": "object", "properties": {}, "additionalProperties": false });
        assert_eq!(
            render_tool_signature(
                &CodemodeToolInfo {
                    name: "mcp__sample__search".to_owned(),
                    input_schema: Some(input_schema.clone()),
                    output_schema: Some(mcp_result_schema(Some(json!({
                        "type": "object",
                        "properties": { "results": { "type": "array", "items": { "$ref": "#/definitions/Result~1item~0v1" } } },
                        "required": ["results"],
                        "additionalProperties": false,
                        "definitions": {
                            "Result/item~v1": {
                                "type": "object",
                                "properties": { "id": { "type": "string" }, "score": { "type": "number" } },
                                "required": ["id", "score"],
                                "additionalProperties": false
                            }
                        }
                    })))),
                    ..Default::default()
                },
                None
            ),
            "mcp__sample__search(args: {}): Promise<CallToolResult<{ results: Array<{ id: string; score: number; }>; }>>;"
        );
        assert_eq!(
            render_tool_signature(
                &CodemodeToolInfo {
                    name: "plain".to_owned(),
                    input_schema: Some(input_schema),
                    output_schema: Some(mcp_result_schema(None)),
                    ..Default::default()
                },
                None
            ),
            "plain(args: {}): Promise<CallToolResult>;"
        );
        assert_eq!(
            mcp_structured_content_schema(Some(
                &json!({ "type": "object", "properties": { "content": { "type": "array" } } })
            )),
            None
        );
    }

    #[test]
    fn renders_the_per_tool_sample() {
        assert_eq!(
            render_tool_sample(&CodemodeToolInfo {
                name: "foo".to_owned(),
                description: Some("bar".to_owned()),
                input_schema: Some(json!({ "type": "string" })),
                ..Default::default()
            }),
            "bar\n\ncodemode tool declaration:\n```ts\ndeclare const tools: { foo(args: string): Promise<unknown>; };\n```"
        );
    }

    #[test]
    fn renders_tools_and_globals() {
        let tools = vec![
            CodemodeToolInfo {
                name: "read".to_owned(),
                description: Some("Read a file.\nSecond line.".to_owned()),
                input_schema: Some(
                    json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
                ),
                output_schema: Some(json!({ "type": "string" })),
                signature: None,
            },
            CodemodeToolInfo {
                name: "remote-api".to_owned(),
                ..Default::default()
            },
        ];
        let globals = vec![CodemodeToolInfo {
            name: "attach".to_owned(),
            description: Some("Attach it.".to_owned()),
            input_schema: Some(json!({ "type": "string" })),
            ..Default::default()
        }];
        let text = render_declarations(RenderDeclarationsOptions {
            tools: &tools,
            globals: &globals,
        });
        assert_eq!(
            text,
            [
                "declare const tools: {",
                "  /**",
                "   * Read a file.",
                "   * Second line.",
                "   */",
                "  read(args: { path: string; }): Promise<string>;",
                "  remote_api(args: unknown): Promise<unknown>;",
                "};",
                "",
                "/** Attach it. */",
                "declare function attach(args: string): Promise<unknown>;",
            ]
            .join("\n")
        );
    }

    #[test]
    fn renders_namespaced_globals_and_explicit_signatures() {
        let globals = vec![
            CodemodeToolInfo {
                name: "models.list".to_owned(),
                description: Some("List models.".to_owned()),
                signature: Some("(type: string): Promise<string[]>".to_owned()),
                ..Default::default()
            },
            CodemodeToolInfo {
                name: "models.get".to_owned(),
                input_schema: Some(json!({ "type": "string" })),
                ..Default::default()
            },
            CodemodeToolInfo {
                name: "plain".to_owned(),
                signature: Some("(): void".to_owned()),
                ..Default::default()
            },
        ];
        let text = render_declarations(RenderDeclarationsOptions {
            tools: &[],
            globals: &globals,
        });
        assert_eq!(
            text,
            [
                "declare function plain(): void;",
                "",
                "declare const models: {",
                "  /** List models. */",
                "  list(type: string): Promise<string[]>;",
                "  get(args: string): Promise<unknown>;",
                "};",
            ]
            .join("\n")
        );
    }

    #[test]
    fn escapes_comment_terminators_in_descriptions() {
        let tools = vec![CodemodeToolInfo {
            name: "x".to_owned(),
            description: Some("a */ b".to_owned()),
            ..Default::default()
        }];
        let text = render_declarations(RenderDeclarationsOptions {
            tools: &tools,
            globals: &[],
        });
        assert!(text.contains("/** a *\\/ b */"));
    }
}
