//! Shared System One classification transport (port of
//! `packages/ai/src/api/system-one-shared.ts` @ abe508e1b).
//!
//! TypeSafe's System One protocol is served by several providers
//! (typesafe / openrouter / vercel-ai-gateway / opencode) and by Cloudflare
//! Workers AI behind its REST envelope. This module owns the request shape,
//! retry/cancel semantics, `noul` bool mapping, and response parsing; each
//! transport supplies only url/payload/output differences.
//!
//! Never rejects: failures come back as a `ClassifierResult` with
//! `stopReason: "error"` (or `"aborted"`) and `errorMessage`.

use serde_json::{Map, Value, json};
use url::Url;

use crate::api::classifier_shared::{
    parse_classifier_usage, post_classifier_request, required_number,
};
use crate::types::{
    ClassifierAnswer, ClassifierBoolAnswer, ClassifierChoiceAnswer, ClassifierContext,
    ClassifierModel, ClassifierOptions, ClassifierQuestion, ClassifierResult,
    ClassifierScoreAnswer, ClassifierStopReason,
};

/// TypeSafe System One request body without the transport-specific envelope.
#[derive(Debug, Clone)]
pub struct SystemOneWireRequest {
    pub state: Map<String, Value>,
    pub questions: Map<String, Value>,
}

/// Differences between services that serve System One models.
pub struct SystemOneTransport {
    /// Classifier API implemented by this transport.
    pub api: &'static str,
    /// Service name used in error messages.
    pub label: &'static str,
    /// Absolute request URL.
    pub url: fn(&ClassifierModel) -> Result<Url, String>,
    /// Wraps the System One request in the service's request envelope.
    pub payload: fn(&ClassifierModel, SystemOneWireRequest) -> Value,
    /// Extracts the System One output (`{ answers, usage }`) from the
    /// service's response envelope.
    pub output: fn(&Value) -> Result<Value, String>,
}

fn probabilities(
    label: &str,
    value: &Value,
    id: &str,
) -> Result<std::collections::BTreeMap<String, f64>, String> {
    let Some(object) = value.as_object() else {
        return Err(format!("{label} returned invalid probabilities for {id}"));
    };
    let mut result = std::collections::BTreeMap::new();
    for (key, probability) in object {
        result.insert(
            key.clone(),
            required_number(label, probability, &format!("probability for {id}.{key}"))?,
        );
    }
    Ok(result)
}

fn parse_answers(
    label: &str,
    value: &Value,
    context: &ClassifierContext,
) -> Result<std::collections::BTreeMap<String, ClassifierAnswer>, String> {
    let Some(object) = value.as_object() else {
        return Err(format!("{label} returned an unexpected response"));
    };
    let mut answers = std::collections::BTreeMap::new();
    for (id, question) in &context.questions {
        let Some(answer) = object.get(id) else {
            return Err(format!("{label} did not return an answer for {id}"));
        };
        match question {
            ClassifierQuestion::Choice(_) => {
                let answer_type = answer.get("type").and_then(Value::as_str);
                if answer_type != Some("choice") {
                    return Err(format!("{label} did not return a choice answer for {id}"));
                }
                let choice = answer
                    .get("choice")
                    .and_then(Value::as_str)
                    .ok_or_else(|| format!("{label} did not return a choice answer for {id}"))?;
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Choice(ClassifierChoiceAnswer {
                        choice: choice.to_owned(),
                        probabilities: probabilities(
                            label,
                            answer.get("probabilities").unwrap_or(&Value::Null),
                            id,
                        )?,
                        confidence: required_number(
                            label,
                            answer.get("confidence").unwrap_or(&Value::Null),
                            &format!("confidence for {id}"),
                        )?,
                    }),
                );
            }
            ClassifierQuestion::Score(_) => {
                if answer.get("type").and_then(Value::as_str) != Some("score") {
                    return Err(format!("{label} did not return a score answer for {id}"));
                }
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Score(ClassifierScoreAnswer {
                        score: required_number(
                            label,
                            answer.get("score").unwrap_or(&Value::Null),
                            &format!("score for {id}"),
                        )?,
                        confidence: required_number(
                            label,
                            answer.get("confidence").unwrap_or(&Value::Null),
                            &format!("confidence for {id}"),
                        )?,
                    }),
                );
            }
            ClassifierQuestion::Bool(_) => {
                // Wire-level `noul`.
                if answer.get("type").and_then(Value::as_str) != Some("noul") {
                    return Err(format!("{label} did not return a bool answer for {id}"));
                }
                answers.insert(
                    id.clone(),
                    ClassifierAnswer::Bool(ClassifierBoolAnswer {
                        probability: required_number(
                            label,
                            answer.get("noul").unwrap_or(&Value::Null),
                            &format!("probability for {id}"),
                        )?,
                    }),
                );
            }
        }
    }
    Ok(answers)
}

/// Maps public `bool` questions to TypeSafe's wire-level `noul` type.
fn wire_request(context: &ClassifierContext) -> SystemOneWireRequest {
    let state = context.state.clone();
    let questions = context
        .questions
        .iter()
        .map(|(id, question)| {
            let value = match question {
                ClassifierQuestion::Choice(question) => json!({
                    "type": "choice",
                    "instructions": question.instructions,
                    "criteria": question.criteria,
                }),
                ClassifierQuestion::Score(question) => json!({
                    "type": "score",
                    "instructions": question.instructions,
                    "criteria": question.criteria,
                }),
                ClassifierQuestion::Bool(question) => json!({
                    "type": "noul",
                    "instructions": question.instructions,
                    "criteria": { "true": question.criteria.yes, "false": question.criteria.no },
                }),
            };
            (id.clone(), value)
        })
        .collect();
    SystemOneWireRequest { state, questions }
}

/// Runs one System One classification over the given transport
/// (`classifySystemOne`). Never rejects.
pub async fn classify_system_one(
    transport: &SystemOneTransport,
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
) -> ClassifierResult {
    let mut output = ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers: std::collections::BTreeMap::new(),
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: now_millis(),
    };
    match classify_inner(transport, model, context, options, &mut output).await {
        Ok(()) => output,
        Err(message) => {
            output.stop_reason = if options
                .and_then(|options| options.signal.as_ref())
                .is_some_and(|signal| signal.is_cancelled())
            {
                ClassifierStopReason::Aborted
            } else {
                ClassifierStopReason::Error
            };
            output.error_message = Some(message);
            output
        }
    }
}

async fn classify_inner(
    transport: &SystemOneTransport,
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
    output: &mut ClassifierResult,
) -> Result<(), String> {
    if model.api.as_str() != transport.api {
        return Err(format!("Unsupported classifier API: {}", model.api));
    }
    // ce8972a0e: System One transports cannot send images; the runtime
    // rejects them before the request instead of sending a malformed body.
    if context
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty())
    {
        return Err(format!("{} does not support image input", transport.label));
    }
    let url = (transport.url)(model)?;
    let payload = (transport.payload)(model, wire_request(context));
    let (body, _) = post_classifier_request(transport.label, url, model, payload, options, &[])
        .await
        .map_err(|error| error.message)?;
    let result = (transport.output)(&body)?;
    // Set before parsing answers: a request with malformed answers was still
    // billed.
    output.usage = parse_classifier_usage(result.get("usage"), model);
    output.answers = parse_answers(
        transport.label,
        result.get("answers").unwrap_or(&Value::Null),
        context,
    )?;
    Ok(())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// Helper for transports that expose the standard `{ model, ...request }`
/// envelope (TypeSafe's native protocol).
pub fn typesafe_payload(model: &ClassifierModel, request: SystemOneWireRequest) -> Value {
    let mut object = Map::new();
    object.insert("model".to_owned(), Value::String(model.id.clone()));
    object.insert("state".to_owned(), Value::Object(request.state));
    object.insert("questions".to_owned(), Value::Object(request.questions));
    Value::Object(object)
}
