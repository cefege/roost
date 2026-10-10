//! TypeSafe System One and chat-model judgment client.
//! Native classifier calls share the System One wire shape; chat fallback uses
//! the same resolved provider account and requires strict JSON answers.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::{
    auth::ResolvedAuth,
    catalog::{ModelInfo, WireApi},
    endpoints::Endpoints,
    error::LlmError,
    message::{AssistantBlock, ChatRequest, Message},
    pool::AccountPool,
};

/// The System One question kinds. `criteria` is what the classifier scores
/// against, so every kind carries it; a choice's options are its criteria keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum QuestionKind {
    Choice {
        criteria: BTreeMap<String, String>,
    },
    Bool {
        when_true: String,
        when_false: String,
    },
    Score {
        criteria: Vec<String>,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub instructions: String,
    #[serde(flatten)]
    pub kind: QuestionKind,
}

impl Question {
    pub fn choice(id: &str, instructions: &str, criteria: &[(&str, &str)]) -> Self {
        Self {
            id: id.to_owned(),
            instructions: instructions.to_owned(),
            kind: QuestionKind::Choice {
                criteria: criteria
                    .iter()
                    .map(|(option, meaning)| ((*option).to_owned(), (*meaning).to_owned()))
                    .collect(),
            },
        }
    }

    pub fn boolean(id: &str, instructions: &str, when_true: &str, when_false: &str) -> Self {
        Self {
            id: id.to_owned(),
            instructions: instructions.to_owned(),
            kind: QuestionKind::Bool {
                when_true: when_true.to_owned(),
                when_false: when_false.to_owned(),
            },
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Answer {
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Bool {
        probability: f64,
    },
    Score {
        score: f64,
        confidence: f64,
    },
}

#[derive(Debug)]
pub struct Judge {
    http: reqwest::Client,
    endpoints: Endpoints,
    pool: Arc<AccountPool>,
}
impl Judge {
    pub fn new(http: reqwest::Client, endpoints: Endpoints, pool: Arc<AccountPool>) -> Self {
        Self {
            http,
            endpoints,
            pool,
        }
    }

    pub async fn judge(
        &self,
        model: &ModelInfo,
        state: serde_json::Value,
        questions: Vec<Question>,
    ) -> Result<BTreeMap<String, Answer>, LlmError> {
        let (_, auth) = self.pool.resolve(&model.provider, "judge").await?;
        if model.api == WireApi::TypesafeSystemOne
            || (model.provider == "openrouter" && model.id.contains("jev"))
        {
            return self.judge_native(model, state, questions, auth).await;
        }
        self.judge_chat(model, state, questions, auth).await
    }

    async fn judge_native(
        &self,
        model: &ModelInfo,
        state: serde_json::Value,
        questions: Vec<Question>,
        auth: ResolvedAuth,
    ) -> Result<BTreeMap<String, Answer>, LlmError> {
        let base = self.endpoints.base(&model.provider, &model.base_url);
        // TypeSafe serves `/v1/systemone`; OpenRouter serves the same wire as
        // its Decisions API, which lives outside the `/v1` prefix.
        let url = if model.provider == "openrouter" {
            let root = base.strip_suffix("/v1").unwrap_or(&base);
            format!("{root}/alpha/decisions")
        } else {
            let root = base.strip_suffix("/v1").unwrap_or(&base);
            format!("{root}/v1/systemone")
        };
        let wire_questions = questions_wire(&questions);
        let body = serde_json::json!({"state":state,"model":model.id,"questions":wire_questions});
        for attempt in 0..3 {
            let (status, headers, bytes) = tokio::time::timeout(Duration::from_secs(10), async {
                let response = self
                    .http
                    .post(&url)
                    .bearer_auth(auth.secret())
                    .json(&body)
                    .send()
                    .await?;
                let status = response.status();
                let headers = response.headers().clone();
                let bytes = response.bytes().await?;
                Ok::<_, reqwest::Error>((status, headers, bytes))
            })
            .await
            .map_err(|_| LlmError::Network("judge request timed out".into()))?
            .map_err(LlmError::from)?;
            if status.is_success() {
                let value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|error| LlmError::Decode(error.to_string()))?;
                let answers = value
                    .get("answers")
                    .cloned()
                    .ok_or_else(|| LlmError::Decode("judge response missing answers".into()))?;
                return parse_answers(answers, &questions);
            }
            if status.as_u16() != 408 && status.as_u16() != 429 && status.as_u16() < 500 {
                return Err(http_error(status.as_u16(), &bytes));
            }
            if attempt < 2 {
                let hinted = retry_after_ms(&headers);
                tokio::time::sleep(Duration::from_millis(
                    hinted.unwrap_or(500_u64 << attempt).min(5000),
                ))
                .await;
            } else {
                return Err(if status.as_u16() == 429 {
                    LlmError::RateLimited {
                        retry_after_ms: retry_after_ms(&headers),
                        reset_at_ms: None,
                    }
                } else {
                    http_error(status.as_u16(), &bytes)
                });
            }
        }
        Err(LlmError::Decode("judge retries exhausted".into()))
    }

    async fn judge_chat(
        &self,
        model: &ModelInfo,
        state: serde_json::Value,
        questions: Vec<Question>,
        auth: ResolvedAuth,
    ) -> Result<BTreeMap<String, Answer>, LlmError> {
        let prompt = format!(
            "Answer each question about the state. Return only a JSON object mapping each question id to a typed answer: `noul` (yes/no) questions use {{\"type\":\"bool\",\"probability\":0..1}} (probability the true criterion holds); choice questions use {{\"type\":\"choice\",\"choice\":<one criteria key>,\"probabilities\":{{<key>:0..1}},\"confidence\":0..1}}; score questions use {{\"type\":\"score\",\"score\":<0-based criteria index>,\"confidence\":0..1}}.\nState: {}\nQuestions: {}",
            state,
            questions_wire(&questions)
        );
        let request = ChatRequest {
            model: model.clone(),
            system: vec!["You are a precise classifier. Return strict JSON only.".into()],
            messages: vec![Message::user_text(prompt)],
            tools: Vec::new(),
            thinking: "off".into(),
            session_id: "judge".into(),
            max_tokens: Some(2048),
        };
        let events = crate::providers::stream_chat(
            &self.http,
            &self.endpoints,
            request,
            auth,
            CancellationToken::new(),
            None,
        );
        futures::pin_mut!(events);
        let mut text = String::new();
        while let Some(event) = futures::StreamExt::next(&mut events).await {
            if let crate::message::StreamEvent::BlockEnd {
                block: AssistantBlock::Text { text: value },
                ..
            } = event?
            {
                text.push_str(&value);
            }
        }
        // Chat models often wrap the object in a code fence or a sentence.
        let object = match (text.find('{'), text.rfind('}')) {
            (Some(start), Some(end)) if start < end => &text[start..=end],
            _ => text.as_str(),
        };
        let value: serde_json::Value =
            serde_json::from_str(object).map_err(|error| LlmError::Decode(error.to_string()))?;
        parse_answers(value, &questions)
    }
}

fn questions_wire(questions: &[Question]) -> serde_json::Value {
    let mut output = serde_json::Map::new();
    for question in questions {
        let value = match &question.kind {
            QuestionKind::Choice { criteria } => serde_json::json!({
                "type": "choice",
                "instructions": question.instructions,
                "criteria": criteria,
            }),
            QuestionKind::Bool {
                when_true,
                when_false,
            } => serde_json::json!({
                "type": "noul",
                "instructions": question.instructions,
                "criteria": {"true": when_true, "false": when_false},
            }),
            QuestionKind::Score { criteria } => serde_json::json!({
                "type": "score",
                "instructions": question.instructions,
                "criteria": criteria,
            }),
        };
        output.insert(question.id.clone(), value);
    }
    serde_json::Value::Object(output)
}
fn parse_answers(
    raw: serde_json::Value,
    questions: &[Question],
) -> Result<BTreeMap<String, Answer>, LlmError> {
    let values = raw
        .as_object()
        .ok_or_else(|| LlmError::Decode("judge answers must be an object".into()))?;
    let mut parsed = BTreeMap::new();
    for question in questions {
        let answer = values
            .get(&question.id)
            .ok_or_else(|| LlmError::Decode(format!("judge omitted answer {}", question.id)))?;
        let number = |key: &str| {
            answer
                .get(key)
                .and_then(serde_json::Value::as_f64)
                .filter(|value| value.is_finite())
                .ok_or_else(|| LlmError::Decode(format!("invalid {key} for {}", question.id)))
        };
        let parsed_answer = match &question.kind {
            QuestionKind::Bool { .. } => {
                if answer.get("type").and_then(serde_json::Value::as_str) != Some("noul")
                    && answer.get("type").and_then(serde_json::Value::as_str) != Some("bool")
                {
                    return Err(LlmError::Decode(format!(
                        "invalid bool answer for {}",
                        question.id
                    )));
                }
                Answer::Bool {
                    probability: number("noul").or_else(|_| number("probability"))?,
                }
            }
            QuestionKind::Choice { .. } => {
                let choice = answer
                    .get("choice")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| LlmError::Decode(format!("invalid choice for {}", question.id)))?
                    .to_owned();
                let probabilities = answer
                    .get("probabilities")
                    .and_then(serde_json::Value::as_object)
                    .ok_or_else(|| {
                        LlmError::Decode(format!("invalid probabilities for {}", question.id))
                    })?
                    .iter()
                    .map(|(key, value)| {
                        value
                            .as_f64()
                            .map(|value| (key.clone(), value))
                            .ok_or_else(|| {
                                LlmError::Decode(format!("invalid probability for {}", question.id))
                            })
                    })
                    .collect::<Result<_, _>>()?;
                Answer::Choice {
                    choice,
                    probabilities,
                    confidence: number("confidence")?,
                }
            }
            QuestionKind::Score { .. } => Answer::Score {
                score: number("score")?,
                confidence: number("confidence")?,
            },
        };
        parsed.insert(question.id.clone(), parsed_answer);
    }
    Ok(parsed)
}
fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1000))
}
fn http_error(status: u16, bytes: &[u8]) -> LlmError {
    let body = String::from_utf8_lossy(bytes).into_owned();
    if matches!(status, 401 | 403) {
        LlmError::Auth(format!("HTTP {status}: {body}"))
    } else {
        LlmError::Http { status, body }
    }
}
