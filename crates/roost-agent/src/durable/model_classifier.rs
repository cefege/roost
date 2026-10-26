//! Routes Pi classifier calls through Roost's account-aware judge.
//! The adapter converts typed questions and answers without owning transport.
//! Native input is text-only; unsupported request options return an error.

use std::{collections::BTreeMap, future::Future, pin::Pin};

use roost_llm::judge::{Answer, Question, QuestionKind};
use rpi_ai::types::{
    ClassifierAnswer, ClassifierBoolAnswer, ClassifierContext, ClassifierModel, ClassifierOptions,
    ClassifierQuestion, ClassifierResult, ClassifierScoreAnswer, ClassifierStopReason,
    ProviderClassifier, classifier_error_result,
};

use crate::runtime::AgentRuntime;

pub(super) struct NativeClassifier {
    runtime: AgentRuntime,
}

impl NativeClassifier {
    pub(super) fn new(runtime: AgentRuntime) -> Self {
        Self { runtime }
    }
}

impl ProviderClassifier for NativeClassifier {
    fn classify(
        &self,
        model: &ClassifierModel,
        context: &ClassifierContext,
        options: Option<&ClassifierOptions>,
    ) -> Pin<Box<dyn Future<Output = ClassifierResult> + Send + 'static>> {
        let runtime = self.runtime.clone();
        let model = model.clone();
        let context = context.clone();
        let options = options.cloned();
        Box::pin(async move { classify(runtime, model, context, options).await })
    }
}

async fn classify(
    runtime: AgentRuntime,
    model: ClassifierModel,
    context: ClassifierContext,
    options: Option<ClassifierOptions>,
) -> ClassifierResult {
    let Some(info) = runtime
        .inner
        .llm
        .catalog()
        .get(&model.provider, &model.id)
        .cloned()
    else {
        return classifier_error_result(
            &model,
            "classifier is absent from the Roost catalog",
            false,
        );
    };
    if context
        .images
        .as_ref()
        .is_some_and(|images| !images.is_empty())
    {
        return classifier_error_result(&model, "native classifiers accept text input only", false);
    }
    let signal = options.as_ref().and_then(|options| options.signal.clone());
    if let Some(error) = unsupported_options(options.as_ref()) {
        return classifier_error_result(&model, error, false);
    }
    let questions = match native_questions(context.questions) {
        Ok(questions) => questions,
        Err(error) => return classifier_error_result(&model, &error, false),
    };
    let state = serde_json::Value::Object(context.state);
    let judge = runtime.inner.llm.judge(&info, state, questions);
    let answers = match signal {
        Some(signal) => tokio::select! {
            _ = signal.cancelled() => {
                return classifier_error_result(&model, "classifier request aborted", true);
            }
            result = judge => result,
        },
        None => judge.await,
    };
    match answers {
        Ok(answers) => classifier_result(&model, answers),
        Err(error) => classifier_error_result(&model, &error.to_string(), false),
    }
}

fn unsupported_options(options: Option<&ClassifierOptions>) -> Option<&'static str> {
    let options = options?;
    if options.api_key.is_some()
        || options.fetch.is_some()
        || options.env.is_some()
        || options.on_payload.is_some()
        || options.on_response.is_some()
        || options.headers.is_some()
        || options.timeout_ms.is_some()
        || options.max_retries.is_some()
        || options.max_retry_delay_ms.is_some()
        || options.temperature.is_some()
    {
        Some("native classifier does not support per-request transport options")
    } else {
        None
    }
}

fn native_questions(
    questions: BTreeMap<String, ClassifierQuestion>,
) -> Result<Vec<Question>, String> {
    questions
        .into_iter()
        .map(|(id, question)| {
            let question = match question {
                ClassifierQuestion::Choice(question) => Question {
                    id,
                    instructions: question.instructions,
                    kind: QuestionKind::Choice {
                        criteria: question.criteria,
                    },
                },
                ClassifierQuestion::Score(question) => Question {
                    id,
                    instructions: question.instructions,
                    kind: QuestionKind::Score {
                        criteria: question.criteria,
                    },
                },
                ClassifierQuestion::Bool(question) => Question::boolean(
                    &id,
                    &question.instructions,
                    &question.criteria.yes,
                    &question.criteria.no,
                ),
            };
            Ok(question)
        })
        .collect()
}

fn classifier_result(
    model: &ClassifierModel,
    answers: BTreeMap<String, Answer>,
) -> ClassifierResult {
    let answers = answers
        .into_iter()
        .map(|(id, answer)| {
            let answer = match answer {
                Answer::Choice {
                    choice,
                    probabilities,
                    confidence,
                } => ClassifierAnswer::Choice(rpi_ai::types::ClassifierChoiceAnswer {
                    choice,
                    probabilities,
                    confidence,
                }),
                Answer::Score { score, confidence } => {
                    ClassifierAnswer::Score(ClassifierScoreAnswer { score, confidence })
                }
                Answer::Bool { probability } => {
                    ClassifierAnswer::Bool(ClassifierBoolAnswer { probability })
                }
            };
            (id, answer)
        })
        .collect();
    ClassifierResult {
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        answers,
        usage: None,
        stop_reason: ClassifierStopReason::Stop,
        error_message: None,
        timestamp: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis() as i64)
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpi_ai::types::{
        ClassifierBoolCriteria, ClassifierBoolQuestion, ClassifierChoiceQuestion,
        ClassifierScoreQuestion,
    };

    #[test]
    fn native_questions_preserve_question_ids_instructions_and_criteria() {
        let questions = BTreeMap::from([
            (
                "choose".to_owned(),
                ClassifierQuestion::Choice(ClassifierChoiceQuestion {
                    instructions: "pick one".to_owned(),
                    criteria: BTreeMap::from([("a".to_owned(), "first".to_owned())]),
                }),
            ),
            (
                "score".to_owned(),
                ClassifierQuestion::Score(ClassifierScoreQuestion {
                    instructions: "rate it".to_owned(),
                    criteria: vec!["clarity".to_owned()],
                }),
            ),
            (
                "boolean".to_owned(),
                ClassifierQuestion::Bool(ClassifierBoolQuestion {
                    instructions: "is it valid?".to_owned(),
                    criteria: ClassifierBoolCriteria {
                        yes: "valid".to_owned(),
                        no: "invalid".to_owned(),
                    },
                }),
            ),
        ]);
        let converted = native_questions(questions).unwrap_or_default();
        assert_eq!(converted.len(), 3);
        assert_eq!(converted[0].id, "boolean");
        assert_eq!(converted[0].instructions, "is it valid?");
        assert_eq!(
            converted[0].kind,
            QuestionKind::Bool {
                when_true: "valid".to_owned(),
                when_false: "invalid".to_owned(),
            }
        );
        assert_eq!(converted[1].id, "choose");
        assert_eq!(converted[1].instructions, "pick one");
        assert_eq!(
            converted[1].kind,
            QuestionKind::Choice {
                criteria: BTreeMap::from([("a".to_owned(), "first".to_owned())]),
            }
        );
        assert_eq!(converted[2].id, "score");
        assert_eq!(converted[2].instructions, "rate it");
        assert_eq!(
            converted[2].kind,
            QuestionKind::Score {
                criteria: vec!["clarity".to_owned()],
            }
        );
    }

    #[test]
    fn transport_options_are_rejected_instead_of_dropped() {
        assert!(unsupported_options(None).is_none());
        assert!(unsupported_options(Some(&ClassifierOptions::default())).is_none());
        assert_eq!(
            unsupported_options(Some(&ClassifierOptions {
                temperature: Some(0.5),
                ..Default::default()
            })),
            Some("native classifier does not support per-request transport options")
        );
    }
}
