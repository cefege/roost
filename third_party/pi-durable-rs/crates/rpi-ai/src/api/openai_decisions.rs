//! OpenAI Decisions classification (port of
//! `packages/ai/src/api/openai-decisions.ts` @ `abe508e1b`, `ce8972a0e`).
//!
//! `POST /v1/decisions` with `{ model, input, questions }`. The state is sent
//! as JSON text; with images the input becomes one user message with the
//! state as `input_text` followed by `input_image` data URLs. Questions map
//! to Decisions types: `choice` to `choice`, `score` to `score`, and `bool`
//! to `predicate` — predicates have no criteria field, so the meanings of
//! true and false are appended to the instructions.
//!
//! Only OpenAI API keys work: Sign in with ChatGPT tokens are rejected on
//! this route (the provider hides classifier models under OAuth).

use std::sync::Arc;

use serde_json::{Map, Value, json};
use url::Url;

use crate::api::classifier_shared::{
    is_record, parse_classifier_usage, post_classifier_request, required_number,
};
use crate::types::{
    ClassifierAnswer, ClassifierBoolAnswer, ClassifierChoiceAnswer, ClassifierContext,
    ClassifierModel, ClassifierOptions, ClassifierQuestion, ClassifierResult,
    ClassifierScoreAnswer, ClassifierStopReason, ProviderClassifier,
};

pub const OPENAI_DECISIONS_API: &str = "openai-decisions";

const LABEL: &str = "OpenAI Decisions";

/// The endpoint accepts at most this many image parts per request.
const MAX_IMAGES: usize = 128;

/// Cloudflare in front of api.openai.com answers 504 with an HTML page when a
/// request runs longer than about five seconds. Large inputs, currently above
/// roughly 600K tokens, hit this limit, and retrying the same input runs into
/// it again, so 504 is not retried.
const NO_RETRY_STATUSES: [u16; 1] = [504];

fn predicate_instructions(question: &crate::types::ClassifierBoolQuestion) -> String {
    let mut meanings = Vec::new();
    if !question.criteria.yes.is_empty() {
        meanings.push(format!("True means: {}", question.criteria.yes));
    }
    if !question.criteria.no.is_empty() {
        meanings.push(format!("False means: {}", question.criteria.no));
    }
    if meanings.is_empty() {
        question.instructions.clone()
    } else {
        format!("{}\n\n{}", question.instructions, meanings.join("\n"))
    }
}

fn wire_question(name: &str, question: &ClassifierQuestion) -> Value {
    match question {
        ClassifierQuestion::Choice(question) => {
            let choices: Vec<Value> = question
                .criteria
                .iter()
                .map(|(value, description)| {
                    if description.is_empty() {
                        json!({"value": value})
                    } else {
                        json!({"value": value, "description": description})
                    }
                })
                .collect();
            json!({
                "type": "choice",
                "name": name,
                "instructions": question.instructions,
                "choices": choices,
            })
        }
        ClassifierQuestion::Score(question) => {
            let levels: Vec<Value> = question
                .criteria
                .iter()
                .map(|label| json!({"label": label}))
                .collect();
            json!({
                "type": "score",
                "name": name,
                "instructions": question.instructions,
                "levels": levels,
            })
        }
        ClassifierQuestion::Bool(question) => json!({
            "type": "predicate",
            "name": name,
            "instructions": predicate_instructions(question),
        }),
    }
}

fn wire_input(context: &ClassifierContext) -> Result<Value, String> {
    let state = serde_json::to_string(&context.state)
        .map_err(|error| format!("{LABEL} could not serialize the state: {error}"))?;
    let images = context.images.clone().unwrap_or_default();
    if images.is_empty() {
        return Ok(Value::String(state));
    }
    if images.len() > MAX_IMAGES {
        return Err(format!(
            "{LABEL} accepts at most {MAX_IMAGES} images, got {}",
            images.len()
        ));
    }
    let mut content = vec![json!({"type": "input_text", "text": state})];
    for image in images {
        content.push(json!({
            "type": "input_image",
            "image_url": format!("data:{};base64,{}", image.mime_type, image.data),
        }));
    }
    Ok(json!([{"role": "user", "content": content}]))
}

fn choice_probabilities(
    label: &str,
    value: &Value,
    id: &str,
) -> Result<std::collections::BTreeMap<String, f64>, String> {
    let Some(entries) = value.as_array() else {
        return Err(format!("{label} returned invalid probabilities for {id}"));
    };
    let mut probabilities = std::collections::BTreeMap::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            return Err(format!("{label} returned invalid probabilities for {id}"));
        };
        let Some(value) = object.get("value").and_then(Value::as_str) else {
            return Err(format!("{label} returned invalid probabilities for {id}"));
        };
        probabilities.insert(
            value.to_owned(),
            required_number(
                label,
                object.get("probability").unwrap_or(&Value::Null),
                &format!("probability for {id}.{value}"),
            )?,
        );
    }
    Ok(probabilities)
}

fn parse_answer(
    id: &str,
    question: &ClassifierQuestion,
    answer: &Map<String, Value>,
) -> Result<ClassifierAnswer, String> {
    if let Some("refusal") = answer.get("type").and_then(Value::as_str) {
        return Err(format!("{LABEL} refused to answer {id}"));
    }
    match question {
        ClassifierQuestion::Choice(_) => {
            if answer.get("type").and_then(Value::as_str) != Some("choice") {
                return Err(format!("{LABEL} did not return a choice answer for {id}"));
            }
            let Some(choice) = answer.get("choice").and_then(Value::as_str) else {
                return Err(format!("{LABEL} did not return a choice answer for {id}"));
            };
            Ok(ClassifierAnswer::Choice(ClassifierChoiceAnswer {
                choice: choice.to_owned(),
                probabilities: choice_probabilities(
                    LABEL,
                    answer.get("probabilities").unwrap_or(&Value::Null),
                    id,
                )?,
                confidence: required_number(
                    LABEL,
                    answer.get("confidence").unwrap_or(&Value::Null),
                    &format!("confidence for {id}"),
                )?,
            }))
        }
        ClassifierQuestion::Score(_) => {
            if answer.get("type").and_then(Value::as_str) != Some("score") {
                return Err(format!("{LABEL} did not return a score answer for {id}"));
            }
            Ok(ClassifierAnswer::Score(ClassifierScoreAnswer {
                score: required_number(
                    LABEL,
                    answer.get("score").unwrap_or(&Value::Null),
                    &format!("score for {id}"),
                )?,
                confidence: required_number(
                    LABEL,
                    answer.get("confidence").unwrap_or(&Value::Null),
                    &format!("confidence for {id}"),
                )?,
            }))
        }
        ClassifierQuestion::Bool(_) => {
            if answer.get("type").and_then(Value::as_str) != Some("predicate") {
                return Err(format!(
                    "{LABEL} did not return a predicate answer for {id}"
                ));
            }
            Ok(ClassifierAnswer::Bool(ClassifierBoolAnswer {
                probability: required_number(
                    LABEL,
                    answer.get("probability").unwrap_or(&Value::Null),
                    &format!("probability for {id}"),
                )?,
            }))
        }
    }
}

fn parse_answers(
    value: &Value,
    context: &ClassifierContext,
) -> Result<std::collections::BTreeMap<String, ClassifierAnswer>, String> {
    let Some(entries) = value.as_array() else {
        return Err(format!("{LABEL} returned an unexpected response"));
    };
    let mut by_name: std::collections::HashMap<&str, &Map<String, Value>> =
        std::collections::HashMap::new();
    for answer in entries {
        if let Some(object) = answer.as_object()
            && let Some(name) = object.get("name").and_then(Value::as_str)
        {
            by_name.insert(name, object);
        }
    }
    let mut answers = std::collections::BTreeMap::new();
    for (id, question) in &context.questions {
        let Some(answer) = by_name.get(id.as_str()) else {
            return Err(format!("{LABEL} did not return an answer for {id}"));
        };
        answers.insert(id.clone(), parse_answer(id, question, answer)?);
    }
    Ok(answers)
}

fn error_message(error: &crate::api::classifier_shared::ClassifierPostError) -> String {
    if error.status == Some(504) {
        return format!(
            "{LABEL} error (504): the request timed out at the gateway. Very large inputs (above roughly 600K tokens) currently exceed its time limit."
        );
    }
    format!("{LABEL} error: {}", error.message)
}

/// Classification through OpenAI's Decisions API. Never rejects.
pub async fn classify(
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
    let result = classify_inner(model, context, options, &mut output).await;
    if let Err(message) = result {
        output.stop_reason = if options
            .and_then(|options| options.signal.as_ref())
            .is_some_and(|signal| signal.is_cancelled())
        {
            ClassifierStopReason::Aborted
        } else {
            ClassifierStopReason::Error
        };
        output.error_message = Some(message);
    }
    output
}

async fn classify_inner(
    model: &ClassifierModel,
    context: &ClassifierContext,
    options: Option<&ClassifierOptions>,
    output: &mut ClassifierResult,
) -> Result<(), String> {
    if model.api.as_str() != OPENAI_DECISIONS_API {
        return Err(format!("Unsupported classifier API: {}", model.api));
    }
    let url = Url::parse(&format!(
        "{}/decisions",
        model.base_url.trim_end_matches('/')
    ))
    .map_err(|error| format!("{LABEL} has an invalid base URL: {error}"))?;
    let questions: Vec<Value> = context
        .questions
        .iter()
        .map(|(id, question)| wire_question(id, question))
        .collect();
    let body = json!({
        "model": model.id,
        "input": wire_input(context)?,
        "questions": questions,
    });
    let (body, _) = post_classifier_request(LABEL, url, model, body, options, &NO_RETRY_STATUSES)
        .await
        .map_err(|error| error_message(&error))?;
    if !is_record(&body) {
        return Err(format!("{LABEL} returned an unexpected response"));
    }
    // Set before parsing answers: a request with malformed or refused
    // answers was still billed.
    output.usage = parse_classifier_usage(body.get("usage"), model);
    output.answers = parse_answers(body.get("answers").unwrap_or(&Value::Null), context)?;
    Ok(())
}

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

/// The `openai-decisions` api implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAiDecisions;

impl ProviderClassifier for OpenAiDecisions {
    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ClassifierResult> + Send + 'static>>
    {
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned();
        Box::pin(async move { classify(&model, &context, options.as_ref()).await })
    }
}

/// `openAIDecisionsApi()`.
pub fn openai_decisions_api() -> Arc<dyn ProviderClassifier> {
    Arc::new(OpenAiDecisions)
}
