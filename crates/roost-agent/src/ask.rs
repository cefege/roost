//! Ported from oh-my-pi packages/coding-agent/src/tools/ask.ts (MIT).
//! Records multi-choice questions as an interactive card and resumes a run
//! when their answers arrive from the client.

use std::collections::BTreeMap;

use roost_protocol::wire::agent_chat::AskQuestion;
use serde::Deserialize;
use serde_json::Value;

use crate::error::AgentError;
use crate::journal;
use crate::records::Entry;
use crate::runtime::{AgentRuntime, RunEnd, Steer, random_id};
use crate::tool_round::{CallResult, CallScope};
use crate::traits::ToolOutcome;

#[derive(Debug, Deserialize)]
struct AskArgs {
    questions: Vec<AskQuestion>,
}

pub(crate) async fn run(scope: &CallScope<'_>, args: &str) -> CallResult {
    let mut value = match serde_json::from_str::<Value>(args) {
        Ok(value) => value,
        Err(error) => return CallResult::failure(format!("Invalid ask arguments: {error}")),
    };
    let Some(questions) = value.get_mut("questions").and_then(Value::as_array_mut) else {
        return CallResult::failure("ask requires a non-empty `questions` array.");
    };
    if questions.is_empty() {
        return CallResult::failure("ask requires a non-empty `questions` array.");
    }
    for (index, question) in questions.iter_mut().enumerate() {
        let Some(question) = question.as_object_mut() else {
            return CallResult::failure("Each ask question must be an object.");
        };
        if question
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| id.trim().is_empty())
        {
            question.insert("id".into(), Value::String(format!("q{}", index + 1)));
        }
    }
    let parsed: AskArgs = match serde_json::from_value(value) {
        Ok(parsed) => parsed,
        Err(error) => return CallResult::failure(format!("Invalid ask arguments: {error}")),
    };
    let mut question_ids = std::collections::HashSet::new();
    for question in &parsed.questions {
        if !question_ids.insert(question.id.as_str()) {
            return CallResult::failure("Ask question ids must be unique.");
        }
        if question.question.trim().is_empty() || !(2..=5).contains(&question.options.len()) {
            return CallResult::failure(
                "Each ask question needs text and between 2 and 5 options.",
            );
        }
        if question
            .recommended
            .is_some_and(|index| index >= question.options.len())
        {
            return CallResult::failure(
                "A recommended option index must point to an available option.",
            );
        }
    }
    let item_id = random_id("ask-");
    let question_count = parsed.questions.len();
    if let Err(error) = scope
        .runtime
        .append(
            &scope.record.id,
            Entry::Ask {
                item_id: item_id.clone(),
                questions: parsed.questions,
                answers: None,
            },
        )
        .await
    {
        return CallResult::failure(error.to_string());
    }
    tracing::info!(conversation_id = %scope.record.id, item_id, question_count, "agent ask submitted");
    CallResult {
        outcome: ToolOutcome {
            is_error: false,
            content: "Questions submitted. Stop and wait for the user's answers.".into(),
            details_json: "{}".into(),
        },
        end: Some(RunEnd::Completed),
    }
}

impl AgentRuntime {
    /// Records answers for an outstanding ask card and resumes the conversation.
    pub async fn answer(
        &self,
        id: &str,
        item_id: &str,
        answers: BTreeMap<String, Vec<String>>,
    ) -> Result<(), AgentError> {
        let entries = self.inner.store.entries(id).await?;
        let entries = journal::effective_entries(&entries);
        let latest = entries.iter().rev().find_map(|(_, entry)| match entry {
            Entry::Ask {
                item_id: found,
                questions,
                answers,
            } if found == item_id => Some((questions.clone(), answers.is_some())),
            _ => None,
        });
        let Some((questions, already_answered)) = latest else {
            return Err(AgentError::NotFound(format!("ask {item_id}")));
        };
        if already_answered {
            return Err(AgentError::FailedPrecondition(
                "this ask was already answered".into(),
            ));
        }
        if answers
            .keys()
            .any(|key| !questions.iter().any(|question| question.id == *key))
        {
            return Err(AgentError::InvalidArgument(
                "answer includes an unknown question id".into(),
            ));
        }
        self.append(
            id,
            Entry::Ask {
                item_id: item_id.to_owned(),
                questions,
                answers: Some(answers.clone()),
            },
        )
        .await?;
        tracing::info!(
            conversation_id = id,
            item_id,
            answered = answers.len(),
            "agent ask answered"
        );
        let steer_text = "The user answered the outstanding questions. Continue using the recorded <answers> context.".to_owned();
        let pending = self.with_run(id, |run| {
            run.steer.send(Steer::User(steer_text.clone())).is_ok()
        });
        if pending != Some(true) {
            self.start_run(id, None).await?;
        }
        Ok(())
    }
}
