//! Durable host effects invoked by portable code-mode events.
//!
//! Child tools and paid model calls use stable per-call keys; pure globals are
//! stored in the result journal and never re-evaluated during replay.

use std::sync::Arc;

use rpi_codemode::portable::Outcome;
use serde_json::{Map, Value};
use tokio::sync::Semaphore;

use crate::chord::context::Context;
use crate::chord::json::JsonObject;
use crate::harness::codemode::models;
use crate::harness::codemode::state::{DeclaredTool, GlobalReceipt, JournalEntry};
use crate::harness::types::{NestedToolExecutionResult, NestedToolOptions, ToolExecutionApi};

pub(super) struct HostCall {
    pub(super) id: u32,
    pub(super) name: String,
    pub(super) args: Option<Value>,
    pub(super) global: Option<String>,
}

pub(super) struct CallCompletion {
    pub(super) entry: JournalEntry,
    pub(super) outcome: Outcome,
    pub(super) generated_images: usize,
}

pub(super) async fn execute(
    call: HostCall,
    tools: &[DeclaredTool],
    api: Arc<dyn ToolExecutionApi>,
    context: Context,
    model_slots: Arc<Semaphore>,
) -> Result<CallCompletion, String> {
    let outcome = if call.global.is_some() {
        execute_global(&call, tools, Arc::clone(&api), context.clone(), model_slots).await?
    } else {
        execute_tool(&call, tools, api.as_ref(), &context).await?
    };
    let generated_images =
        if call.name == "models.generateImages" || call.name == models::GENERATE_IMAGES_TOOL {
            match &outcome {
                Outcome::Json(value) => count_images(value),
                Outcome::Error { properties, .. } => properties
                    .as_ref()
                    .and_then(|properties| properties.get("structuredOutput"))
                    .map_or(0, count_images),
                Outcome::Undefined => 0,
            }
        } else {
            0
        };
    let at = api.now().map_err(|error| error.to_string())?;
    let at = time_ms(at)?;
    let global = call.global.map(|_| GlobalReceipt {
        name: call.name,
        args: call.args,
        outcome: outcome_to_json(&outcome),
    });
    Ok(CallCompletion {
        entry: JournalEntry {
            id: call.id,
            at,
            issued_after: None,
            global,
        },
        outcome,
        generated_images,
    })
}

async fn execute_tool(
    call: &HostCall,
    tools: &[DeclaredTool],
    api: &dyn ToolExecutionApi,
    context: &Context,
) -> Result<Outcome, String> {
    let arguments = match call.args.as_ref() {
        None | Some(Value::Null) => JsonObject::new(),
        Some(Value::Object(arguments)) => arguments.clone(),
        Some(_) => {
            return Ok(error_outcome(
                "Tool arguments must be an object".to_string(),
            ));
        }
    };
    let result = api
        .execute_tool(
            &call.name,
            arguments,
            NestedToolOptions {
                key: Some(format!("c{}", call.id)),
                progress: Some(true),
            },
            context,
        )
        .await
        .map_err(|error| error.to_string())?;
    let structured = tools
        .iter()
        .any(|tool| tool.name == call.name && tool.structured);
    nested_outcome(&call.name, structured, result)
}

async fn execute_global(
    call: &HostCall,
    tools: &[DeclaredTool],
    api: Arc<dyn ToolExecutionApi>,
    context: Context,
    model_slots: Arc<Semaphore>,
) -> Result<Outcome, String> {
    match call.name.as_str() {
        "models.classify" | "models.generateImages" => {
            let _permit = model_slots
                .acquire()
                .await
                .map_err(|error| error.to_string())?;
            let Some(arguments) = call.args.as_ref().and_then(Value::as_array) else {
                return Ok(error_outcome(format!(
                    "{}(model, context) expects two arguments",
                    call.name
                )));
            };
            if arguments.len() != 2 {
                return Ok(error_outcome(format!(
                    "{}(model, context) expects two arguments",
                    call.name
                )));
            }
            let tool_name = if call.name == "models.classify" {
                models::CLASSIFY_TOOL
            } else {
                models::GENERATE_IMAGES_TOOL
            };
            let model_call = HostCall {
                id: call.id,
                name: tool_name.to_string(),
                args: Some(serde_json::json!({"model": arguments[0], "context": arguments[1]})),
                global: None,
            };
            match execute_tool(&model_call, tools, api.as_ref(), &context).await {
                Ok(outcome) => Ok(outcome),
                Err(message) => Ok(error_outcome(message)),
            }
        }
        "models.getModelsOfType" | "models.getAvailableOfType" | "models.getModelOfType" => {
            let arguments = match call.args.as_ref() {
                None => &[][..],
                Some(Value::Array(arguments)) => arguments.as_slice(),
                Some(_) => {
                    return Ok(error_outcome(format!(
                        "{} expects an argument array",
                        call.name
                    )));
                }
            };
            match models::catalog_call(&call.name, arguments, &api.models(), &context).await {
                Ok(Some(value)) => Ok(Outcome::Json(value)),
                Ok(None) => Ok(Outcome::Undefined),
                Err(message) => Ok(error_outcome(message)),
            }
        }
        name => Ok(error_outcome(format!("Unknown global {name}"))),
    }
}

fn error_outcome(message: String) -> Outcome {
    Outcome::Error {
        message,
        properties: None,
    }
}

fn nested_outcome(
    name: &str,
    structured: bool,
    result: NestedToolExecutionResult,
) -> Result<Outcome, String> {
    let value = result.structured_output.map(|output| {
        if structured {
            output
        } else {
            match output {
                Value::String(text) => Value::String(text),
                other => Value::String(text_of(&other)),
            }
        }
    });
    if !result.is_error || (structured && value.is_some()) {
        return Ok(value.map_or(Outcome::Undefined, Outcome::Json));
    }
    let diagnostics = result
        .diagnostics
        .iter()
        .filter(|diagnostic| {
            diagnostic.severity == crate::harness::types::DiagnosticSeverity::Error
        })
        .map(|diagnostic| diagnostic.message.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let message = value
        .as_ref()
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .or_else(|| (!diagnostics.is_empty()).then_some(diagnostics.as_str()))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("Tool {name} failed"));
    let mut properties = Map::new();
    if let Some(value) = value {
        properties.insert("structuredOutput".into(), value);
    }
    if let Some(details) = result.details {
        properties.insert("details".into(), details);
    }
    if let Some(usage) = result.usage {
        properties.insert(
            "usage".into(),
            serde_json::to_value(usage).map_err(|error| error.to_string())?,
        );
    }
    properties.insert(
        "diagnostics".into(),
        serde_json::to_value(result.diagnostics).map_err(|error| error.to_string())?,
    );
    Ok(Outcome::Error {
        message,
        properties: Some(properties),
    })
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                (item.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| item.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn count_images(value: &Value) -> usize {
    match value {
        Value::Object(object) => {
            usize::from(object.get("type").and_then(Value::as_str) == Some("image"))
                + object.values().map(count_images).sum::<usize>()
        }
        Value::Array(values) => values.iter().map(count_images).sum(),
        _ => 0,
    }
}

fn time_ms(now: f64) -> Result<u64, String> {
    if !now.is_finite() || now < 0.0 || now > u64::MAX as f64 {
        return Err("Code-mode clock returned an invalid millisecond timestamp".to_string());
    }
    Ok(now as u64)
}

pub(super) fn outcome_to_json(outcome: &Outcome) -> Value {
    let mut object = Map::new();
    match outcome {
        Outcome::Undefined => {
            object.insert("ok".to_string(), Value::Bool(true));
        }
        Outcome::Json(value) => {
            object.insert("ok".to_string(), Value::Bool(true));
            object.insert("value".to_string(), value.clone());
        }
        Outcome::Error {
            message,
            properties,
        } => {
            object.insert("ok".to_string(), Value::Bool(false));
            object.insert("message".to_string(), Value::String(message.clone()));
            if let Some(properties) = properties {
                object.insert("properties".into(), Value::Object(properties.clone()));
            }
        }
    }
    Value::Object(object)
}

pub(super) fn outcome_from_json(value: &Value) -> Result<Outcome, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Malformed durable global outcome".to_string())?;
    if object.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(match object.get("value") {
            Some(value) => Outcome::Json(value.clone()),
            None => Outcome::Undefined,
        });
    }
    if object.get("ok").and_then(Value::as_bool) != Some(false) {
        return Err("Malformed durable global outcome".to_string());
    }
    let properties = match object.get("properties") {
        None => None,
        Some(Value::Object(properties)) => Some(properties.clone()),
        _ => return Err("Malformed durable global error properties".into()),
    };
    let message = object
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| "Malformed durable global error message".to_string())?;
    Ok(Outcome::Error {
        message: message.to_owned(),
        properties,
    })
}
